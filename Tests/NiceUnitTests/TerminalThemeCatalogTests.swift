//
//  TerminalThemeCatalogTests.swift
//  NiceUnitTests
//
//  Unit tests for `TerminalThemeCatalog` — built-in inventory, scope
//  filtering, and import / remove flows against an injected temp
//  support directory.
//

import Foundation
import SwiftUI
import XCTest
@testable import Nice

@MainActor
final class TerminalThemeCatalogTests: XCTestCase {

    private var tempDir: URL!

    override func setUp() async throws {
        try await super.setUp()
        tempDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("NiceThemeCatalogTests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
    }

    override func tearDown() async throws {
        try? FileManager.default.removeItem(at: tempDir)
        try await super.tearDown()
    }

    // MARK: - Built-ins

    func test_builtInList_containsExpectedIds() {
        let catalog = TerminalThemeCatalog(supportDirectory: tempDir)
        let ids = Set(catalog.builtIn.map(\.id))
        let expected: Set<String> = [
            "nice-default-light",
            "nice-default-dark",
            "solarized-light",
            "solarized-dark",
            "dracula",
            "nord",
            "gruvbox-light",
            "gruvbox-dark",
            "catppuccin-latte",
            "catppuccin-mocha",
            "tokyo-night",
            "one-dark",
        ]
        XCTAssertTrue(expected.isSubset(of: ids), "Missing built-in ids: \(expected.subtracting(ids))")
    }

    func test_allBuiltIns_have16AnsiEntries() {
        let catalog = TerminalThemeCatalog(supportDirectory: tempDir)
        for theme in catalog.builtIn {
            XCTAssertEqual(theme.ansi.count, 16, "Theme \(theme.id) has \(theme.ansi.count) ANSI entries")
        }
    }

    func test_niceDefaults_haveNilCursor() {
        let catalog = TerminalThemeCatalog(supportDirectory: tempDir)
        let light = catalog.theme(withId: "nice-default-light")
        let dark = catalog.theme(withId: "nice-default-dark")
        XCTAssertNotNil(light)
        XCTAssertNotNil(dark)
        XCTAssertNil(light?.cursor, "Nice Default (Light) should have cursor=nil so the accent drives the caret")
        XCTAssertNil(dark?.cursor, "Nice Default (Dark) should have cursor=nil so the accent drives the caret")
    }

    func test_draculaBuiltIn_hasCanonicalCursor() {
        let catalog = TerminalThemeCatalog(supportDirectory: tempDir)
        guard let dracula = catalog.theme(withId: "dracula") else {
            return XCTFail("Dracula missing from built-ins")
        }
        XCTAssertEqual(dracula.cursor, ThemeColor(hex: "f8f8f2"))
    }

    // MARK: - Scope filtering

    func test_themesForLight_filtersByScope() {
        let catalog = TerminalThemeCatalog(supportDirectory: tempDir)
        let lightThemes = catalog.themes(for: .light)
        XCTAssertFalse(lightThemes.contains { $0.id == "dracula" }, "Dracula (.dark) leaked into light picker")
        XCTAssertTrue(lightThemes.contains { $0.id == "nice-default-light" })
        XCTAssertTrue(lightThemes.contains { $0.id == "solarized-light" })
    }

    func test_themesForDark_filtersByScope() {
        let catalog = TerminalThemeCatalog(supportDirectory: tempDir)
        let darkThemes = catalog.themes(for: .dark)
        XCTAssertFalse(darkThemes.contains { $0.id == "solarized-light" }, "Solarized Light (.light) leaked into dark picker")
        XCTAssertTrue(darkThemes.contains { $0.id == "nice-default-dark" })
        XCTAssertTrue(darkThemes.contains { $0.id == "dracula" })
    }

    // MARK: - Import

    func test_importedTheme_loadedFromDiskOnInit() throws {
        let sourceFile = tempDir.appendingPathComponent("custom-theme.ghostty")
        try Self.minimalThemeSource().write(to: sourceFile, atomically: true, encoding: .utf8)

        let catalog = TerminalThemeCatalog(supportDirectory: tempDir)
        XCTAssertEqual(catalog.imported.count, 1)
        XCTAssertEqual(catalog.imported.first?.id, "custom-theme")
    }

    func test_import_copiesFileAndAppends() throws {
        let catalog = TerminalThemeCatalog(supportDirectory: tempDir)
        let externalDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("NiceThemeCatalogTests-external-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: externalDir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: externalDir) }

        let externalFile = externalDir.appendingPathComponent("catppuccin_frappe.ghostty")
        try Self.minimalThemeSource().write(to: externalFile, atomically: true, encoding: .utf8)

        let theme = try catalog.importTheme(from: externalFile)
        XCTAssertEqual(theme.id, "catppuccin-frappe")
        XCTAssertEqual(theme.displayName, "Catppuccin Frappe")
        XCTAssertTrue(catalog.imported.contains { $0.id == theme.id })
        let persistedPath = tempDir.appendingPathComponent("catppuccin-frappe.ghostty")
        XCTAssertTrue(FileManager.default.fileExists(atPath: persistedPath.path))
    }

    func test_import_duplicateId_replaces() throws {
        let catalog = TerminalThemeCatalog(supportDirectory: tempDir)

        let firstFile = FileManager.default.temporaryDirectory
            .appendingPathComponent("duplicate.ghostty")
        try Self.minimalThemeSource(background: "111111").write(to: firstFile, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: firstFile) }

        let first = try catalog.importTheme(from: firstFile)
        XCTAssertEqual(first.background, ThemeColor(hex: "111111"))

        try Self.minimalThemeSource(background: "222222").write(to: firstFile, atomically: true, encoding: .utf8)
        let second = try catalog.importTheme(from: firstFile)

        XCTAssertEqual(second.id, first.id)
        XCTAssertEqual(second.background, ThemeColor(hex: "222222"))
        XCTAssertEqual(catalog.imported.filter { $0.id == first.id }.count, 1, "Duplicate import should replace, not duplicate")
    }

    func test_import_invalidFile_throwsParseError() throws {
        let catalog = TerminalThemeCatalog(supportDirectory: tempDir)
        let badFile = FileManager.default.temporaryDirectory
            .appendingPathComponent("bad-\(UUID().uuidString).ghostty")
        try "background = 1d1f21\nforeground = c5c8c6".write(
            to: badFile, atomically: true, encoding: .utf8
        )
        defer { try? FileManager.default.removeItem(at: badFile) }

        XCTAssertThrowsError(try catalog.importTheme(from: badFile)) { error in
            guard case TerminalThemeCatalog.ImportError.parseFailed = error else {
                return XCTFail("Expected .parseFailed, got \(error)")
            }
        }
        XCTAssertTrue(catalog.imported.isEmpty)
    }

    // MARK: - Remove

    func test_remove_deletesFileAndUpdatesList() throws {
        let sourceFile = tempDir.appendingPathComponent("ephemeral.ghostty")
        try Self.minimalThemeSource().write(to: sourceFile, atomically: true, encoding: .utf8)

        let catalog = TerminalThemeCatalog(supportDirectory: tempDir)
        guard let theme = catalog.imported.first else {
            return XCTFail("Expected one imported theme after init")
        }

        try catalog.remove(theme)
        XCTAssertTrue(catalog.imported.isEmpty)
        XCTAssertFalse(FileManager.default.fileExists(atPath: sourceFile.path))
    }

    func test_remove_builtIn_isNoOp() throws {
        let catalog = TerminalThemeCatalog(supportDirectory: tempDir)
        guard let dracula = catalog.theme(withId: "dracula") else {
            return XCTFail("Dracula missing")
        }
        let originalCount = catalog.builtIn.count
        try catalog.remove(dracula)
        XCTAssertEqual(catalog.builtIn.count, originalCount, "Built-ins should not be removable")
    }

    // MARK: - Slug / display name helpers

    func test_slug_normalizesCasingAndSeparators() {
        XCTAssertEqual(TerminalThemeCatalog.slug(from: "Catppuccin Frappe"), "catppuccin-frappe")
        XCTAssertEqual(TerminalThemeCatalog.slug(from: "catppuccin_frappe"), "catppuccin-frappe")
        XCTAssertEqual(TerminalThemeCatalog.slug(from: "catppuccin-frappe"), "catppuccin-frappe")
        XCTAssertEqual(TerminalThemeCatalog.slug(from: "Tokyo Night!"), "tokyo-night")
    }

    func test_displayName_titleCasesWords() {
        XCTAssertEqual(TerminalThemeCatalog.displayName(from: "catppuccin-frappe"), "Catppuccin Frappe")
        XCTAssertEqual(TerminalThemeCatalog.displayName(from: "tokyo_night"), "Tokyo Night")
    }

    // MARK: - Fixture

    private static func minimalThemeSource(background: String = "1d1f21") -> String {
        var lines = [
            "background = \(background)",
            "foreground = c5c8c6",
        ]
        for i in 0..<16 {
            lines.append("palette = \(i)=#\(String(repeating: "0", count: 5))\(String(i, radix: 16))")
        }
        return lines.joined(separator: "\n") + "\n"
    }
}
