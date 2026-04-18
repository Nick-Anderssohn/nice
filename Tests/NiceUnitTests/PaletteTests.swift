//
//  PaletteTests.swift
//  NiceUnitTests
//
//  Unit tests for Sources/Nice/Theme/Palette.swift — the color helper
//  extensions on `SwiftUI.Color`, the palette `EnvironmentValues` key, and
//  the ANSI palette tables in `NiceANSIPalette`.
//
//  `SwiftUI.Color` doesn't expose a useful `==` that compares resolved
//  values, so we bridge to `NSColor(self)` and compare `.cgColor` component
//  arrays. For the macOS-palette test we pin an explicit `NSAppearance` so
//  dynamic semantic colors resolve to a deterministic CGColor.
//

import AppKit
import SwiftTerm
import SwiftUI
import XCTest
@testable import Nice

@MainActor
final class PaletteTests: XCTestCase {

    // MARK: - Color comparison helpers

    /// Returns the CGColor components of a `SwiftUI.Color` after resolving
    /// against the supplied appearance (default: current).
    private func components(
        of color: SwiftUI.Color,
        appearance: NSAppearance? = nil
    ) -> [CGFloat]? {
        let ns = NSColor(color)
        if let appearance {
            var resolved: [CGFloat]?
            appearance.performAsCurrentDrawingAppearance {
                resolved = ns.cgColor.components
            }
            return resolved
        }
        return ns.cgColor.components
    }

    private func assertSameColor(
        _ a: SwiftUI.Color,
        _ b: SwiftUI.Color,
        appearance: NSAppearance? = nil,
        tolerance: CGFloat = 0.001,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        guard let ac = components(of: a, appearance: appearance),
              let bc = components(of: b, appearance: appearance) else {
            XCTFail("failed to resolve CGColor components", file: file, line: line)
            return
        }
        XCTAssertEqual(ac.count, bc.count,
                       "component counts differ",
                       file: file, line: line)
        for (i, (x, y)) in zip(ac, bc).enumerated() {
            XCTAssertEqual(x, y, accuracy: tolerance,
                           "component \(i) differs: \(x) vs \(y)",
                           file: file, line: line)
        }
    }

    // MARK: - Nice palette: literal colors

    func test_palette_nice_returnsLiteralColors_dark() {
        // Source literal from Palette.swift's `niceBg(_:_:)` dark branch.
        let expected = SwiftUI.Color(.sRGB, red: 0.080, green: 0.066, blue: 0.055, opacity: 1.0)
        let actual = SwiftUI.Color.niceBg(.dark, .nice)
        assertSameColor(actual, expected)
    }

    func test_palette_nice_returnsLiteralColors_light() {
        // Source literal from Palette.swift's `niceBg(_:_:)` light branch.
        let expected = SwiftUI.Color(.sRGB, red: 0.989, green: 0.978, blue: 0.970, opacity: 1.0)
        let actual = SwiftUI.Color.niceBg(.light, .nice)
        assertSameColor(actual, expected)
    }

    // MARK: - macOS palette: system semantic delegation

    func test_palette_macOS_delegatesToSystemSemanticColor() {
        // `niceBg(_:, .macOS)` should return `Color(nsColor: .windowBackgroundColor)`
        // regardless of the scheme argument (the scheme is ignored for the
        // macOS palette — AppKit resolves dynamically against the current
        // NSAppearance). Compare after pinning to darkAqua so the dynamic
        // NSColor resolves to a deterministic CGColor.
        let darkAqua = NSAppearance(named: .darkAqua)!
        let expected = SwiftUI.Color(nsColor: .windowBackgroundColor)
        let actual = SwiftUI.Color.niceBg(.dark, .macOS)
        assertSameColor(actual, expected, appearance: darkAqua)

        // And the same under aqua.
        let aqua = NSAppearance(named: .aqua)!
        let expectedLight = SwiftUI.Color(nsColor: .windowBackgroundColor)
        let actualLight = SwiftUI.Color.niceBg(.light, .macOS)
        assertSameColor(actualLight, expectedLight, appearance: aqua)
    }

    // MARK: - Environment key default

    func test_environmentKey_defaultsToNice() {
        XCTAssertEqual(EnvironmentValues().palette, .nice)
    }

    // MARK: - ANSI palette per-scheme

    func test_ansiPalette_perScheme_returnsDifferentTables() {
        let dark = NiceANSIPalette.colors(for: .dark)
        let light = NiceANSIPalette.colors(for: .light)

        XCTAssertEqual(dark.count, 16, "ANSI palette must have 16 entries")
        XCTAssertEqual(light.count, 16, "ANSI palette must have 16 entries")

        // Tables must differ overall.
        XCTAssertNotEqual(dark, light,
                          "dark and light ANSI tables should differ")

        // Spot-check index 0 (black) differs: dark's black ≈ niceBg3 (very
        // dark brown), light's black = niceInk (near-black but distinct
        // channel values).
        XCTAssertNotEqual(dark[0], light[0],
                          "index 0 should differ between dark and light tables")

        // And index 15 (bright white) — dark's is near-white (niceInk),
        // light's is near-black (niceInk) so "bright white" stays legible.
        XCTAssertNotEqual(dark[15], light[15],
                          "index 15 should differ between dark and light tables")
    }
}
