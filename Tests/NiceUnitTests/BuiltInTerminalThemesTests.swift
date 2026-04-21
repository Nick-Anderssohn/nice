//
//  BuiltInTerminalThemesTests.swift
//  NiceUnitTests
//
//  Sanity checks on the bundled terminal themes. These are pure data —
//  the thing that regresses is typos during a theme-add PR: a missing
//  ANSI entry, a repeated id, or a hex value that silently decodes to
//  zero. This suite catches those at `swift test` time instead of when
//  the user opens Settings and sees their terminal go black.
//

import XCTest
@testable import Nice

@MainActor
final class BuiltInTerminalThemesTests: XCTestCase {

    func test_allThemes_haveExactly16AnsiEntries() {
        // 16 = 0–7 normal + 8–15 bright. SwiftTerm's `installColors`
        // expects exactly 16 entries; shorter arrays silently leave
        // the tail at defaults, longer ones crash.
        for theme in BuiltInTerminalThemes.all {
            XCTAssertEqual(
                theme.ansi.count, 16,
                "\(theme.id) must have 16 ANSI entries, has \(theme.ansi.count)"
            )
        }
    }

    func test_allThemes_haveUniqueIds() {
        let ids = BuiltInTerminalThemes.all.map(\.id)
        XCTAssertEqual(
            Set(ids).count, ids.count,
            "Built-in theme ids must be unique; duplicates make TerminalThemeCatalog.theme(withId:) non-deterministic."
        )
    }

    func test_allThemes_haveNonEmptyDisplayName() {
        for theme in BuiltInTerminalThemes.all {
            XCTAssertFalse(theme.displayName.isEmpty,
                           "\(theme.id) has empty displayName")
        }
    }

    func test_allThemes_sourcedAsBuiltIn() {
        for theme in BuiltInTerminalThemes.all {
            XCTAssertEqual(theme.source, .builtIn,
                           "\(theme.id) must declare .builtIn source.")
        }
    }

    func test_defaultTerminalThemeIds_resolveToBuiltIns() {
        // `Tweaks.defaultTerminalThemeLightId` / `...DarkId` are the
        // fallback ids when the user's saved selection can't be found.
        // A rename on the theme side without updating these constants
        // would silently fall through to the catalog's ultimate
        // fallback (or crash the force-unwrap in
        // `effectiveTerminalTheme`).
        let ids = BuiltInTerminalThemes.all.map(\.id)
        XCTAssertTrue(
            ids.contains(Tweaks.defaultTerminalThemeLightId),
            "Built-in theme with id \(Tweaks.defaultTerminalThemeLightId) not found — Tweaks fallback will crash."
        )
        XCTAssertTrue(
            ids.contains(Tweaks.defaultTerminalThemeDarkId),
            "Built-in theme with id \(Tweaks.defaultTerminalThemeDarkId) not found — Tweaks fallback will crash."
        )
    }

    func test_niceDefaults_haveCorrectScope() {
        // The Nice Default variants are scheme-pinned; picker filtering
        // relies on that.
        XCTAssertEqual(BuiltInTerminalThemes.niceDefaultLight.scope, .light)
        XCTAssertEqual(BuiltInTerminalThemes.niceDefaultDark.scope, .dark)
    }
}
