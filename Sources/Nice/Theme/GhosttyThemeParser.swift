//
//  GhosttyThemeParser.swift
//  Nice
//
//  Parses Ghostty's `key = value` theme files into a `TerminalTheme`.
//  The format is line-oriented; unknown keys are silently ignored so
//  future Ghostty additions don't break imports. The caller owns the
//  theme id and display name — typically derived from the file name.
//

import Foundation

enum GhosttyThemeParser {

    enum ParseError: Error, Equatable {
        /// One or more palette indices in 0..<16 weren't provided.
        case missingPalette(indices: [Int])
        /// `background` or `foreground` wasn't provided (both are required).
        case missingRequiredKey(key: String)
        /// A hex color couldn't be decoded. `lineNumber` is 1-indexed.
        case invalidHex(value: String, lineNumber: Int)
        /// A `palette = N=#...` line had a palette index outside 0..<16.
        case paletteIndexOutOfRange(index: Int, lineNumber: Int)
    }

    /// Parse a Ghostty theme file. `url` is recorded as `source: .imported(url:)`.
    /// The caller is responsible for picking the theme id and display name —
    /// typically derived from the file name (caller's concern).
    static func parse(
        _ source: String,
        id: String,
        displayName: String,
        url: URL
    ) throws -> TerminalTheme {
        var background: ThemeColor?
        var foreground: ThemeColor?
        var cursor: ThemeColor?
        var selection: ThemeColor?
        var palette: [Int: ThemeColor] = [:]

        // Split on LF or CR so CRLF-terminated files also parse. An empty
        // substring between \r and \n simply becomes a blank line and is
        // ignored below.
        let lines = source.split(
            omittingEmptySubsequences: false,
            whereSeparator: { $0 == "\n" || $0 == "\r" }
        )

        for (index, rawLine) in lines.enumerated() {
            let lineNumber = index + 1
            let trimmed = rawLine.trimmingCharacters(in: .whitespaces)
            if trimmed.isEmpty { continue }
            if trimmed.hasPrefix("#") { continue }

            guard let eqIdx = trimmed.firstIndex(of: "=") else { continue }
            let key = trimmed[..<eqIdx].trimmingCharacters(in: .whitespaces)
            let value = trimmed[trimmed.index(after: eqIdx)...]
                .trimmingCharacters(in: .whitespaces)

            switch key {
            case "background":
                guard let color = ThemeColor(hex: value) else {
                    throw ParseError.invalidHex(value: value, lineNumber: lineNumber)
                }
                background = color
            case "foreground":
                guard let color = ThemeColor(hex: value) else {
                    throw ParseError.invalidHex(value: value, lineNumber: lineNumber)
                }
                foreground = color
            case "cursor-color":
                guard let color = ThemeColor(hex: value) else {
                    throw ParseError.invalidHex(value: value, lineNumber: lineNumber)
                }
                cursor = color
            case "selection-background":
                guard let color = ThemeColor(hex: value) else {
                    throw ParseError.invalidHex(value: value, lineNumber: lineNumber)
                }
                selection = color
            case "palette":
                // `N=#rrggbb` or `N=rrggbb`. Split on the first `=` in the value.
                guard let innerEq = value.firstIndex(of: "=") else {
                    throw ParseError.invalidHex(value: value, lineNumber: lineNumber)
                }
                let indexStr = value[..<innerEq].trimmingCharacters(in: .whitespaces)
                let hexStr = value[value.index(after: innerEq)...]
                    .trimmingCharacters(in: .whitespaces)
                guard let paletteIndex = Int(indexStr) else {
                    throw ParseError.invalidHex(value: value, lineNumber: lineNumber)
                }
                guard (0..<16).contains(paletteIndex) else {
                    throw ParseError.paletteIndexOutOfRange(
                        index: paletteIndex,
                        lineNumber: lineNumber
                    )
                }
                guard let color = ThemeColor(hex: hexStr) else {
                    throw ParseError.invalidHex(value: hexStr, lineNumber: lineNumber)
                }
                palette[paletteIndex] = color
            default:
                // Unknown key — silently ignored so the parser tolerates
                // keys Ghostty adds in future versions.
                continue
            }
        }

        // Required keys first, then palette completeness — deterministic error
        // ordering so callers get a stable message regardless of which
        // problem shows up first in the file.
        guard let background else {
            throw ParseError.missingRequiredKey(key: "background")
        }
        guard let foreground else {
            throw ParseError.missingRequiredKey(key: "foreground")
        }

        let missing = (0..<16).filter { palette[$0] == nil }
        if !missing.isEmpty {
            throw ParseError.missingPalette(indices: missing)
        }

        let ansi: [ThemeColor] = (0..<16).map { palette[$0]! }

        return TerminalTheme(
            id: id,
            displayName: displayName,
            scope: .either,
            background: background,
            foreground: foreground,
            cursor: cursor,
            selection: selection,
            ansi: ansi,
            source: .imported(url: url)
        )
    }
}
