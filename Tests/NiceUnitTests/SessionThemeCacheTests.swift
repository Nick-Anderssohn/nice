//
//  SessionThemeCacheTests.swift
//  NiceUnitTests
//
//  Coverage for the four `SessionThemeCache.updateX` theme fan-out
//  paths — `updateScheme`, `updateTerminalFontSize`,
//  `updateTerminalTheme`, `updateTerminalFontFamily` — plus
//  `applyAll(to:)` (the seed call SessionsModel makes against a
//  brand-new `TabPtySession`). Each `updateX` must:
//    1. Update the cached field so a subsequent `applyAll(to:)`
//       seeds a new receiver with the latest value.
//    2. Call the corresponding `applyX` on every receiver returned
//       by the closure passed at init.
//    3. Be safe to call when the receivers list is empty.
//
//  Tests construct `SessionThemeCache` directly with a controllable
//  receivers list and drive the methods. SessionsModel's forwarders
//  (`updateScheme` etc.) are exercised transitively by the broader
//  AppState-rooted tests; the cache-level invariants live here.
//

import AppKit
import Foundation
import SwiftUI
import XCTest
@testable import Nice

@MainActor
final class SessionThemeCacheTests: XCTestCase {

    private var receivers: [any TabPtySessionThemeable] = []
    private var cache: SessionThemeCache!

    override func setUp() {
        super.setUp()
        receivers = []
        cache = SessionThemeCache { [unowned self] in receivers }
    }

    override func tearDown() {
        cache = nil
        receivers = []
        super.tearDown()
    }

    // MARK: - updateScheme

    func test_updateScheme_fansToEveryReceiver() {
        let a = FakeTabPtySession()
        let b = FakeTabPtySession()
        receivers = [a, b]

        let accent = NSColor.systemTeal
        cache.updateScheme(.dark, palette: .macOS, accent: accent)

        XCTAssertEqual(a.applyThemeCalls.count, 1)
        XCTAssertEqual(b.applyThemeCalls.count, 1)
        XCTAssertEqual(a.applyThemeCalls.last?.scheme, .dark)
        XCTAssertEqual(a.applyThemeCalls.last?.palette, .macOS)
        XCTAssertEqual(a.applyThemeCalls.last?.accent, accent)
        XCTAssertEqual(b.applyThemeCalls.last?.palette, .macOS)
    }

    func test_updateScheme_updatesCacheForFutureSeeds() {
        // The cache fields are what `applyAll(to:)` reads when
        // seeding a brand-new `TabPtySession`. Verifying the fields
        // update is equivalent to verifying that a session created
        // after this point would be themed correctly.
        cache.updateScheme(.light, palette: .catppuccinLatte, accent: .systemPink)

        XCTAssertEqual(cache.scheme, .light)
        XCTAssertEqual(cache.palette, .catppuccinLatte)
        XCTAssertEqual(cache.accent, NSColor.systemPink)
    }

    func test_updateScheme_withNoReceivers_doesNotCrash() {
        // Smoke: the update must be safe before any session has been
        // created — `AppState.init` calls these from the Tweaks seed
        // step, before the first `makeSession`.
        XCTAssertNoThrow(
            cache.updateScheme(.dark, palette: .nice, accent: .systemTeal)
        )
        XCTAssertEqual(cache.scheme, .dark,
                       "Cache must update even with no receivers — applyAll runs against future receivers later.")
    }

    // MARK: - updateTerminalFontSize

    func test_updateTerminalFontSize_fansToEveryReceiver() {
        let a = FakeTabPtySession()
        let b = FakeTabPtySession()
        receivers = [a, b]

        cache.updateTerminalFontSize(18.5)

        XCTAssertEqual(a.applyTerminalFontSizeCalls, [18.5])
        XCTAssertEqual(b.applyTerminalFontSizeCalls, [18.5])
        XCTAssertEqual(cache.terminalFontSize, 18.5)
    }

    // MARK: - updateTerminalTheme

    func test_updateTerminalTheme_fansToEveryReceiver() {
        let a = FakeTabPtySession()
        let b = FakeTabPtySession()
        receivers = [a, b]

        let theme = BuiltInTerminalThemes.niceDefaultLight
        cache.updateTerminalTheme(theme)

        XCTAssertEqual(a.applyTerminalThemeCalls.count, 1)
        XCTAssertEqual(b.applyTerminalThemeCalls.count, 1)
        XCTAssertEqual(a.applyTerminalThemeCalls.last, theme)
        XCTAssertEqual(cache.terminalTheme, theme)
    }

    // MARK: - updateTerminalFontFamily

    func test_updateTerminalFontFamily_fansToEveryReceiver() {
        let a = FakeTabPtySession()
        let b = FakeTabPtySession()
        receivers = [a, b]

        cache.updateTerminalFontFamily("JetBrains Mono")

        XCTAssertEqual(a.applyTerminalFontFamilyCalls, ["JetBrains Mono"])
        XCTAssertEqual(b.applyTerminalFontFamilyCalls, ["JetBrains Mono"])
        XCTAssertEqual(cache.terminalFontFamily, "JetBrains Mono")
    }

    func test_updateTerminalFontFamily_nilResetsToDefault() {
        // `nil` is the "use the default chain" sentinel — the cache
        // and every receiver must see it propagate.
        let a = FakeTabPtySession()
        receivers = [a]

        cache.updateTerminalFontFamily("ResetSentinelStart")
        cache.updateTerminalFontFamily(nil)

        XCTAssertEqual(a.applyTerminalFontFamilyCalls, ["ResetSentinelStart", nil])
        XCTAssertNil(cache.terminalFontFamily)
    }

    // MARK: - Receiver list resolution

    func test_receiversClosureReResolvedOnEachUpdate() {
        // The cache asks for the receiver list on every call, so
        // late-arriving receivers participate in the next fan-out
        // without any add/remove notification.
        let early = FakeTabPtySession()
        receivers = [early]
        cache.updateScheme(.dark, palette: .nice, accent: .systemTeal)
        XCTAssertEqual(early.applyThemeCalls.count, 1)

        // Add a late receiver — must not be backfilled with the
        // prior call.
        let late = FakeTabPtySession()
        receivers.append(late)
        XCTAssertTrue(late.applyThemeCalls.isEmpty,
                      "Late registration must not see the previous update.")

        cache.updateScheme(.light, palette: .macOS, accent: .systemPink)
        XCTAssertEqual(early.applyThemeCalls.count, 2,
                       "Existing receiver must keep receiving updates.")
        XCTAssertEqual(late.applyThemeCalls.count, 1,
                       "Late receiver picks up updates from the next call onward.")
        XCTAssertEqual(late.applyThemeCalls.last?.scheme, .light)
    }

    // MARK: - applyAll

    func test_applyAll_seedsReceiverWithEntireCachedState() {
        // Seed the cache with a non-default state, then `applyAll`
        // a fresh receiver — every cached field must land.
        cache.updateScheme(.light, palette: .catppuccinLatte, accent: .systemPink)
        cache.updateTerminalFontSize(15)
        cache.updateTerminalTheme(BuiltInTerminalThemes.niceDefaultLight)
        cache.updateTerminalFontFamily("Menlo")

        let fresh = FakeTabPtySession()
        cache.applyAll(to: fresh)

        XCTAssertEqual(fresh.applyTerminalFontFamilyCalls, ["Menlo"])
        XCTAssertEqual(fresh.applyThemeCalls.count, 1)
        XCTAssertEqual(fresh.applyThemeCalls.last?.scheme, .light)
        XCTAssertEqual(fresh.applyThemeCalls.last?.palette, .catppuccinLatte)
        XCTAssertEqual(fresh.applyTerminalThemeCalls.last,
                       BuiltInTerminalThemes.niceDefaultLight)
        XCTAssertEqual(fresh.applyTerminalFontSizeCalls, [15])
    }

    func test_applyAll_preservesThemeBeforeTerminalThemeOrder() {
        // Order matters: applyTheme must run before
        // applyTerminalTheme so the chrome-coupled bg/fg paths in
        // applyTerminalTheme see the freshly-seeded scheme/palette
        // rather than stale values. A regression that flipped the
        // order would paint a brand-new pane with the wrong
        // light/dark variant.
        let recorder = OrderRecordingThemeable()
        cache.applyAll(to: recorder)

        let order = recorder.callOrder
        guard let themeIdx = order.firstIndex(of: "applyTheme"),
              let termThemeIdx = order.firstIndex(of: "applyTerminalTheme")
        else {
            return XCTFail("Both apply calls must reach the receiver: \(order)")
        }
        XCTAssertLessThan(themeIdx, termThemeIdx,
                          "applyTheme must precede applyTerminalTheme — chrome-coupled bg/fg depends on it.")
    }

    // MARK: - Local helpers

    private final class OrderRecordingThemeable: TabPtySessionThemeable {
        private(set) var callOrder: [String] = []
        func applyTheme(_ scheme: ColorScheme, palette: Palette, accent: NSColor) {
            callOrder.append("applyTheme")
        }
        func applyTerminalFont(size: CGFloat) {
            callOrder.append("applyTerminalFont")
        }
        func applyTerminalTheme(_ theme: TerminalTheme) {
            callOrder.append("applyTerminalTheme")
        }
        func applyTerminalFontFamily(_ name: String?) {
            callOrder.append("applyTerminalFontFamily")
        }
    }
}
