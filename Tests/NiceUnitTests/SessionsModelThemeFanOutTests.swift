//
//  SessionsModelThemeFanOutTests.swift
//  NiceUnitTests
//
//  Coverage for the four `SessionsModel.updateX` theme fan-out paths
//  — `updateScheme`, `updateTerminalFontSize`, `updateTerminalTheme`,
//  `updateTerminalFontFamily`. Each must:
//    1. Update the SessionsModel's cached state so a `makeSession`
//       call after the update seeds new sessions with the latest
//       value.
//    2. Apply the value to every live receiver (`ptySessions` plus
//       the test-only `_testing_themeReceivers` collection).
//    3. Be safe to call when no receivers exist.
//
//  Tests register `FakeTabPtySession`s on
//  `SessionsModel._testing_themeReceivers`, exercise the four update
//  paths, and assert via the fakes' recorded payloads + the
//  `_testing_themeCache` readback.
//

import AppKit
import Foundation
import SwiftUI
import XCTest
@testable import Nice

@MainActor
final class SessionsModelThemeFanOutTests: XCTestCase {

    private var tabs: TabModel!
    private var sessions: SessionsModel!

    override func setUp() {
        super.setUp()
        tabs = TabModel(initialMainCwd: "/tmp/nice-theme-fanout")
        sessions = SessionsModel(tabs: tabs)
    }

    override func tearDown() {
        sessions?.tearDown()
        sessions = nil
        tabs = nil
        super.tearDown()
    }

    // MARK: - updateScheme

    func test_updateScheme_fansToEverySession() {
        let a = FakeTabPtySession()
        let b = FakeTabPtySession()
        sessions._testing_themeReceivers["a"] = a
        sessions._testing_themeReceivers["b"] = b

        let accent = NSColor.systemTeal
        sessions.updateScheme(.dark, palette: .macOS, accent: accent)

        XCTAssertEqual(a.applyThemeCalls.count, 1)
        XCTAssertEqual(b.applyThemeCalls.count, 1)
        XCTAssertEqual(a.applyThemeCalls.last?.scheme, .dark)
        XCTAssertEqual(a.applyThemeCalls.last?.palette, .macOS)
        XCTAssertEqual(a.applyThemeCalls.last?.accent, accent)
        XCTAssertEqual(b.applyThemeCalls.last?.palette, .macOS)
    }

    func test_updateScheme_updatesCacheSoNewSessionsPickItUp() {
        // The cache fields are what `makeSession` reads when seeding
        // a freshly-created `TabPtySession`. Verifying them updates
        // is equivalent to verifying that a session created after
        // this point would be themed correctly.
        sessions.updateScheme(.light, palette: .catppuccinLatte, accent: .systemPink)

        let cache = sessions._testing_themeCache
        XCTAssertEqual(cache.scheme, .light)
        XCTAssertEqual(cache.palette, .catppuccinLatte)
        XCTAssertEqual(cache.accent, NSColor.systemPink)
    }

    func test_updateScheme_withNoReceivers_doesNotCrash() {
        // Smoke: the update must be safe before any session has been
        // created — `AppState.init` calls these from the Tweaks seed
        // step, before the first `makeSession`.
        XCTAssertNoThrow(
            sessions.updateScheme(.dark, palette: .nice, accent: .systemTeal)
        )
    }

    // MARK: - updateTerminalFontSize

    func test_updateTerminalFontSize_fansToEverySession() {
        let a = FakeTabPtySession()
        let b = FakeTabPtySession()
        sessions._testing_themeReceivers["a"] = a
        sessions._testing_themeReceivers["b"] = b

        sessions.updateTerminalFontSize(18.5)

        XCTAssertEqual(a.applyTerminalFontSizeCalls, [18.5])
        XCTAssertEqual(b.applyTerminalFontSizeCalls, [18.5])
        XCTAssertEqual(sessions._testing_themeCache.fontSize, 18.5)
    }

    // MARK: - updateTerminalTheme

    func test_updateTerminalTheme_fansToEverySession() {
        let a = FakeTabPtySession()
        let b = FakeTabPtySession()
        sessions._testing_themeReceivers["a"] = a
        sessions._testing_themeReceivers["b"] = b

        let theme = BuiltInTerminalThemes.niceDefaultLight
        sessions.updateTerminalTheme(theme)

        XCTAssertEqual(a.applyTerminalThemeCalls.count, 1)
        XCTAssertEqual(b.applyTerminalThemeCalls.count, 1)
        XCTAssertEqual(a.applyTerminalThemeCalls.last, theme)
        XCTAssertEqual(sessions._testing_themeCache.theme, theme)
    }

    // MARK: - updateTerminalFontFamily

    func test_updateTerminalFontFamily_fansToEverySession() {
        let a = FakeTabPtySession()
        let b = FakeTabPtySession()
        sessions._testing_themeReceivers["a"] = a
        sessions._testing_themeReceivers["b"] = b

        sessions.updateTerminalFontFamily("JetBrains Mono")

        XCTAssertEqual(a.applyTerminalFontFamilyCalls, ["JetBrains Mono"])
        XCTAssertEqual(b.applyTerminalFontFamilyCalls, ["JetBrains Mono"])
        XCTAssertEqual(sessions._testing_themeCache.fontFamily, "JetBrains Mono")
    }

    func test_updateTerminalFontFamily_nilResetsToDefault() {
        // `nil` is the "use the default chain" sentinel — the cache
        // and every receiver must see it propagate.
        let a = FakeTabPtySession()
        sessions._testing_themeReceivers["a"] = a

        sessions.updateTerminalFontFamily("ResetSentinelStart")
        sessions.updateTerminalFontFamily(nil)

        XCTAssertEqual(a.applyTerminalFontFamilyCalls, ["ResetSentinelStart", nil])
        XCTAssertNil(sessions._testing_themeCache.fontFamily)
    }

    // MARK: - Receiver registration churn

    func test_receiverRegisteredAfterUpdate_isNotBackfilled() {
        // Receivers register *before* the update they care about —
        // there's no replay. A late registration must not see the
        // old call, and the next update must reach it.
        sessions.updateScheme(.dark, palette: .nice, accent: .systemTeal)

        let late = FakeTabPtySession()
        sessions._testing_themeReceivers["late"] = late
        XCTAssertTrue(late.applyThemeCalls.isEmpty,
                      "Late registration must not be backfilled with prior updates.")

        sessions.updateScheme(.light, palette: .macOS, accent: .systemPink)
        XCTAssertEqual(late.applyThemeCalls.count, 1,
                       "Update after registration must reach the late receiver.")
        XCTAssertEqual(late.applyThemeCalls.last?.scheme, .light)
    }
}
