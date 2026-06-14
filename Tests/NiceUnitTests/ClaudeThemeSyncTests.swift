//
//  ClaudeThemeSyncTests.swift
//  NiceUnitTests
//
//  Locks down the Nice→Claude theme mirror:
//  - the JSON Nice writes to ~/.claude/themes/nice.json: base flips with
//    the scheme, the right Nice colors land on the right Claude tokens
//    (including the bright-ANSI emphasis tokens), and a malformed palette
//    degrades to a clean light/dark flip instead of trapping
//  - the on-disk behavior: both files are created, writes are
//    only-if-changed (no churn → Claude's watcher isn't woken needlessly),
//    and a foreign/unparseable nice.json is never clobbered
//  - the color math helpers (blend / lighten / NSColor → ThemeColor)
//
//  Tests pass sandboxed URLs via the `write(...:themesDir:settingsURL:)`
//  surface — no env-var dance, no risk of touching the developer's real
//  ~/.claude/.
//

import AppKit
import SwiftUI
import XCTest
@testable import Nice

final class ClaudeThemeSyncTests: XCTestCase {

    private var tmpRoot: URL!
    private var themesDir: URL!
    private var settingsURL: URL!

    override func setUpWithError() throws {
        tmpRoot = URL(fileURLWithPath: NSTemporaryDirectory(), isDirectory: true)
            .appendingPathComponent("nice-theme-sync-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmpRoot, withIntermediateDirectories: true)
        themesDir = tmpRoot.appendingPathComponent(".claude/themes")
        settingsURL = tmpRoot.appendingPathComponent(".nice/claude-theme-settings.json")
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: tmpRoot)
    }

    // A theme whose ANSI entries encode their index in the red channel
    // (a[i] == #i0000), so assertions can name the index they expect
    // without hard-coding hex. fg/bg/selection get distinctive values.
    private func makeTheme(scope: TerminalTheme.Scope = .either,
                           selection: ThemeColor? = ThemeColor(10, 20, 30)) -> TerminalTheme {
        TerminalTheme(
            id: "test",
            displayName: "Test",
            scope: scope,
            background: ThemeColor(0, 0, 0),
            foreground: ThemeColor(255, 255, 255),
            cursor: nil,
            selection: selection,
            ansi: (0..<16).map { ThemeColor(UInt8($0), 0, 0) },
            source: .builtIn
        )
    }

    private func overrides(_ json: [String: Any]) -> [String: String] {
        (json["overrides"] as? [String: String]) ?? [:]
    }

    // MARK: - base / scheme

    func test_base_flipsWithScheme() {
        let t = makeTheme()
        XCTAssertEqual(
            ClaudeThemeSync.makeThemeJSON(theme: t, scheme: .dark, accent: ThemeColor(1, 2, 3))["base"] as? String,
            "dark"
        )
        XCTAssertEqual(
            ClaudeThemeSync.makeThemeJSON(theme: t, scheme: .light, accent: ThemeColor(1, 2, 3))["base"] as? String,
            "light"
        )
    }

    func test_carriesNameAndManagedMarker() {
        let json = ClaudeThemeSync.makeThemeJSON(theme: makeTheme(), scheme: .dark, accent: ThemeColor(1, 2, 3))
        XCTAssertEqual(json["name"] as? String, "Nice")
        XCTAssertEqual(json[ClaudeThemeSync.managedMarker] as? Bool, true,
                       "managed marker lets us tell our own file apart from a user's")
    }

    // MARK: - token mapping (wiring)

    func test_mapsCoreTokens() {
        let t = makeTheme()
        let accent = ThemeColor(7, 8, 9)
        let o = overrides(ClaudeThemeSync.makeThemeJSON(theme: t, scheme: .dark, accent: accent))

        XCTAssertEqual(o["text"], t.foreground.hexString)
        XCTAssertEqual(o["inverseText"], t.background.hexString)
        XCTAssertEqual(o["background"], t.background.hexString)
        XCTAssertEqual(o["selectionBg"], t.selection?.hexString)
        XCTAssertEqual(o["error"], t.ansi[1].hexString)
        XCTAssertEqual(o["success"], t.ansi[2].hexString)
        XCTAssertEqual(o["warning"], t.ansi[3].hexString)
        XCTAssertEqual(o["claude"], accent.hexString)
        XCTAssertEqual(o["autoAccept"], accent.hexString)
        XCTAssertEqual(o["fastMode"], accent.hexString)
        XCTAssertEqual(o["permission"], t.ansi[4].hexString)
        XCTAssertEqual(o["planMode"], t.ansi[6].hexString)
        XCTAssertEqual(o["bashBorder"], t.ansi[5].hexString)
        XCTAssertEqual(o["inactive"], t.ansi[8].hexString)
    }

    func test_usesBrightAnsiVariants() {
        // The whole point of the bright half of the palette: emphasis
        // tokens must pull from a[8…15], not the normal a[0…7].
        let t = makeTheme()
        let o = overrides(ClaudeThemeSync.makeThemeJSON(theme: t, scheme: .dark, accent: ThemeColor(7, 8, 9)))
        XCTAssertEqual(o["warningShimmer"], t.ansi[11].hexString)
        XCTAssertEqual(o["permissionShimmer"], t.ansi[12].hexString)
        XCTAssertEqual(o["inactiveShimmer"], t.ansi[15].hexString)
        // Word-level diffs blend the BRIGHT red/green over the background.
        XCTAssertEqual(o["diffAddedWord"],
                       ClaudeThemeSync.blend(t.ansi[10], over: t.background, alpha: 0.55).hexString)
        XCTAssertEqual(o["diffRemovedWord"],
                       ClaudeThemeSync.blend(t.ansi[9], over: t.background, alpha: 0.55).hexString)
    }

    func test_blockDiffsBlendNormalAnsiOverBackground() {
        let t = makeTheme()
        let o = overrides(ClaudeThemeSync.makeThemeJSON(theme: t, scheme: .dark, accent: ThemeColor(7, 8, 9)))
        XCTAssertEqual(o["diffAdded"],
                       ClaudeThemeSync.blend(t.ansi[2], over: t.background, alpha: 0.30).hexString)
        XCTAssertEqual(o["diffRemoved"],
                       ClaudeThemeSync.blend(t.ansi[1], over: t.background, alpha: 0.30).hexString)
        XCTAssertEqual(o["diffAddedDimmed"],
                       ClaudeThemeSync.blend(t.ansi[2], over: t.background, alpha: 0.15).hexString)
    }

    func test_shimmerAccentTokensAreLightenedAccent() {
        let accent = ThemeColor(100, 0, 0)
        let o = overrides(ClaudeThemeSync.makeThemeJSON(theme: makeTheme(), scheme: .dark, accent: accent))
        XCTAssertEqual(o["claudeShimmer"], ClaudeThemeSync.lighten(accent, amount: 0.25).hexString)
        XCTAssertEqual(o["fastModeShimmer"], ClaudeThemeSync.lighten(accent, amount: 0.25).hexString)
    }

    func test_omitsSelectionWhenNil() {
        let o = overrides(ClaudeThemeSync.makeThemeJSON(
            theme: makeTheme(selection: nil), scheme: .dark, accent: ThemeColor(1, 2, 3)))
        XCTAssertNil(o["selectionBg"],
                     "no selection color → fall through to base rather than emit garbage")
    }

    func test_malformedAnsi_returnsBaseOnlyWithoutTrapping() {
        // A theme with the wrong ANSI count must degrade to a clean
        // light/dark flip, never index out of bounds.
        let bad = TerminalTheme(
            id: "bad", displayName: "Bad", scope: .either,
            background: ThemeColor(0, 0, 0), foreground: ThemeColor(255, 255, 255),
            cursor: nil, selection: nil,
            ansi: [ThemeColor(1, 1, 1)],   // only 1 entry
            source: .builtIn
        )
        let json = ClaudeThemeSync.makeThemeJSON(theme: bad, scheme: .light, accent: ThemeColor(1, 2, 3))
        XCTAssertEqual(json["base"] as? String, "light")
        XCTAssertNil(json["overrides"], "malformed palette emits no overrides")
    }

    func test_emitsColorsInClaudeAcceptedHexForm() {
        // Every override value must be `#rrggbb` (6 lowercase hex digits),
        // the form Claude's parser accepts; anything else is silently
        // dropped by Claude.
        let o = overrides(ClaudeThemeSync.makeThemeJSON(
            theme: makeTheme(), scheme: .dark, accent: ThemeColor(7, 8, 9)))
        let hex = try? NSRegularExpression(pattern: "^#[0-9a-f]{6}$")
        for (token, value) in o {
            let range = NSRange(value.startIndex..., in: value)
            XCTAssertNotNil(hex?.firstMatch(in: value, range: range),
                            "token \(token) emitted non-hex value \(value)")
        }
    }

    // MARK: - color math

    func test_blend_endpointsAndMidpoint() {
        let black = ThemeColor(0, 0, 0), white = ThemeColor(255, 255, 255)
        XCTAssertEqual(ClaudeThemeSync.blend(white, over: black, alpha: 0.0).hexString, "#000000")
        XCTAssertEqual(ClaudeThemeSync.blend(white, over: black, alpha: 1.0).hexString, "#ffffff")
        XCTAssertEqual(ClaudeThemeSync.blend(white, over: black, alpha: 0.5).hexString, "#808080")
    }

    func test_lighten_movesTowardWhite() {
        XCTAssertEqual(ClaudeThemeSync.lighten(ThemeColor(0, 0, 0), amount: 0.25).hexString, "#404040")
    }

    func test_themeColor_fromNSColor_sRGB() {
        XCTAssertEqual(ClaudeThemeSync.themeColor(NSColor.black), ThemeColor(0, 0, 0))
        XCTAssertEqual(
            ClaudeThemeSync.themeColor(NSColor(srgbRed: 1, green: 0, blue: 0, alpha: 1)),
            ThemeColor(255, 0, 0)
        )
    }

    func test_hexString_formatting() {
        XCTAssertEqual(ThemeColor(0, 0, 0).hexString, "#000000")
        XCTAssertEqual(ThemeColor(255, 255, 255).hexString, "#ffffff")
        XCTAssertEqual(ThemeColor(15, 16, 255).hexString, "#0f10ff")
    }

    // MARK: - file writing

    private func themeFileURL() -> URL { themesDir.appendingPathComponent("nice.json") }

    private func writeOnce(scheme: ColorScheme = .dark) {
        ClaudeThemeSync.write(
            theme: makeTheme(), scheme: scheme, accent: ThemeColor(1, 2, 3),
            themesDir: themesDir, settingsURL: settingsURL
        )
    }

    func test_write_createsBothFiles() throws {
        writeOnce()
        XCTAssertTrue(FileManager.default.fileExists(atPath: themeFileURL().path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: settingsURL.path))
    }

    func test_settingsFile_carriesCustomPointer() throws {
        writeOnce()
        let data = try Data(contentsOf: settingsURL)
        let dict = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(dict["theme"] as? String, "custom:nice")
    }

    func test_write_isOnlyIfChanged() throws {
        writeOnce()
        let m1 = try mtime(of: themeFileURL())
        Thread.sleep(forTimeInterval: 0.05)
        writeOnce()  // identical inputs
        XCTAssertEqual(try mtime(of: themeFileURL()), m1,
                       "an unchanged theme must not rewrite the file (chokidar-safe)")
    }

    func test_write_updatesOnRealChange() throws {
        writeOnce(scheme: .dark)
        ClaudeThemeSync.write(
            theme: makeTheme(), scheme: .light, accent: ThemeColor(1, 2, 3),
            themesDir: themesDir, settingsURL: settingsURL
        )
        let dict = try XCTUnwrap(
            JSONSerialization.jsonObject(with: try Data(contentsOf: themeFileURL())) as? [String: Any]
        )
        XCTAssertEqual(dict["base"] as? String, "light", "a real change must land on disk")
    }

    func test_write_refusesToClobberForeignThemeFile() throws {
        // A nice.json the user hand-authored (no _niceManaged marker) must
        // be left exactly as-is.
        try FileManager.default.createDirectory(at: themesDir, withIntermediateDirectories: true)
        let foreign = Data(#"{"name":"My Theme","base":"dark"}"#.utf8)
        try foreign.write(to: themeFileURL())

        writeOnce()

        XCTAssertEqual(try Data(contentsOf: themeFileURL()), foreign,
                       "a foreign nice.json must not be overwritten")
    }

    func test_write_overwritesOwnManagedFile() throws {
        // First write is ours (marker present) → a later write updates it.
        writeOnce(scheme: .dark)
        ClaudeThemeSync.write(
            theme: makeTheme(), scheme: .light, accent: ThemeColor(9, 9, 9),
            themesDir: themesDir, settingsURL: settingsURL
        )
        let dict = try XCTUnwrap(
            JSONSerialization.jsonObject(with: try Data(contentsOf: themeFileURL())) as? [String: Any]
        )
        XCTAssertEqual(dict["base"] as? String, "light",
                       "our own managed file is fair game to update")
    }

    func test_write_refusesToClobberNonJSONThemeFile() throws {
        try FileManager.default.createDirectory(at: themesDir, withIntermediateDirectories: true)
        let garbage = Data("not json {{{".utf8)
        try garbage.write(to: themeFileURL())

        writeOnce()

        XCTAssertEqual(try Data(contentsOf: themeFileURL()), garbage,
                       "an unparseable file must be left intact, never destroyed")
    }

    func test_defaultThemesDir_endsInThemes() {
        XCTAssertEqual(ClaudeThemeSync.defaultThemesDir().lastPathComponent, "themes")
    }

    func test_defaultThemeSettingsURL_underNiceDir() {
        // Handed to `claude --settings`; lives under the no-space ~/.nice
        // dir and is named distinctly from Claude's own settings.json.
        let url = ClaudeThemeSync.defaultThemeSettingsURL()
        XCTAssertEqual(url.lastPathComponent, "claude-theme-settings.json")
        XCTAssertEqual(url.deletingLastPathComponent().lastPathComponent, ".nice")
    }

    private func mtime(of url: URL) throws -> Date {
        let attrs = try FileManager.default.attributesOfItem(atPath: url.path)
        return try XCTUnwrap(attrs[.modificationDate] as? Date)
    }
}
