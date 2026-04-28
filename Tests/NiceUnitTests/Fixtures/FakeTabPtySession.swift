//
//  FakeTabPtySession.swift
//  NiceUnitTests
//
//  Records every theme-fan-out call routed through
//  `TabPtySessionThemeable` so unit tests can verify that
//  `SessionsModel.updateScheme` / `updateTerminalFontSize` /
//  `updateTerminalTheme` / `updateTerminalFontFamily` reach every
//  registered receiver. Production sessions stay in
//  `ptySessions` (real `TabPtySession`s); fakes are wired through
//  `SessionsModel._testing_themeReceivers`, which the four update
//  methods walk alongside the real session map.
//

import AppKit
import SwiftUI
@testable import Nice

@MainActor
final class FakeTabPtySession: TabPtySessionThemeable {
    /// Every applyTheme call's payload, in arrival order. Tests
    /// assert on `last` plus call count to pin down both fan-out
    /// breadth and ordering.
    private(set) var applyThemeCalls: [(
        scheme: ColorScheme, palette: Palette, accent: NSColor
    )] = []
    private(set) var applyTerminalFontSizeCalls: [CGFloat] = []
    private(set) var applyTerminalThemeCalls: [TerminalTheme] = []
    private(set) var applyTerminalFontFamilyCalls: [String?] = []

    func applyTheme(_ scheme: ColorScheme, palette: Palette, accent: NSColor) {
        applyThemeCalls.append((scheme, palette, accent))
    }

    func applyTerminalFont(size: CGFloat) {
        applyTerminalFontSizeCalls.append(size)
    }

    func applyTerminalTheme(_ theme: TerminalTheme) {
        applyTerminalThemeCalls.append(theme)
    }

    func applyTerminalFontFamily(_ name: String?) {
        applyTerminalFontFamilyCalls.append(name)
    }
}
