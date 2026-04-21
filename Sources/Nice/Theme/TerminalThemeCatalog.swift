//
//  TerminalThemeCatalog.swift
//  Nice
//
//  The runtime list of terminal themes: Nice's bundled built-ins
//  plus anything the user has imported from Ghostty theme files.
//  Imported themes live as files under `supportDirectory` so users
//  can share, version-control, or hand-edit them.
//

import Combine
import Foundation
import SwiftUI

@MainActor
final class TerminalThemeCatalog: ObservableObject {

    /// Frozen list of Nice's own themes.
    let builtIn: [TerminalTheme] = BuiltInTerminalThemes.all

    /// Themes parsed from `supportDirectory`. Mutated by `importTheme`
    /// and `remove`. Published so the Settings UI rebuilds when it changes.
    @Published private(set) var imported: [TerminalTheme] = []

    /// Directory scanned for `.ghostty` / `.conf` files. Injectable so
    /// tests can point at a temp dir instead of the real Application
    /// Support path.
    let supportDirectory: URL

    init(supportDirectory: URL) {
        self.supportDirectory = supportDirectory
        reloadImported()
    }

    /// Default Application Support path: `~/Library/Application Support/<CFBundleName>/terminal-themes/`.
    /// Creates the directory if missing. Falls back to a temporary
    /// directory if the standard path is unavailable — in that rare
    /// case imports won't persist, but the app still functions.
    static func defaultSupportDirectory() -> URL {
        let fm = FileManager.default
        let base: URL
        if let appSupport = try? fm.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        ) {
            base = appSupport
        } else {
            base = fm.temporaryDirectory
        }
        // Folder name tracks CFBundleName so the `Nice Dev` variant
        // keeps its imported themes separate from the user's real
        // `…/Nice/terminal-themes/`.
        let folder = (Bundle.main.object(forInfoDictionaryKey: "CFBundleName") as? String) ?? "Nice"
        let dir = base.appendingPathComponent("\(folder)/terminal-themes", isDirectory: true)
        try? fm.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    // MARK: - Queries

    /// Themes that belong in the picker for `scheme`. `.either`-scope
    /// themes (all imports) appear in both. Built-ins come first, then
    /// imports — both blocks sorted by display name for stable ordering.
    func themes(for scheme: ColorScheme) -> [TerminalTheme] {
        let builtInMatches = builtIn.filter { $0.matches(scheme: scheme) }
        let importedMatches = imported.filter { $0.matches(scheme: scheme) }
        return builtInMatches + importedMatches.sorted { $0.displayName < $1.displayName }
    }

    /// Lookup by id. Used by the resolver when the persisted
    /// `terminalThemeLightId` / `DarkId` doesn't match any current
    /// theme — the caller then falls back to a Nice Default.
    func theme(withId id: String) -> TerminalTheme? {
        if let b = builtIn.first(where: { $0.id == id }) { return b }
        return imported.first(where: { $0.id == id })
    }

    // MARK: - Mutation

    enum ImportError: Error, Equatable {
        /// File couldn't be read (I/O error, permissions, etc.). The
        /// underlying NSError's localizedDescription rides along so
        /// the UI can surface something actionable.
        case cannotRead(message: String)
        /// Parser rejected the file. The inner error preserves the
        /// specific validation failure.
        case parseFailed(GhosttyThemeParser.ParseError)
        /// The copy-into-support-directory step failed.
        case cannotPersist(message: String)
    }

    /// Import `url` into the catalog: parse, copy into the support
    /// directory, add to `imported`. If a theme with the same id
    /// already exists, it is replaced (both in `imported` and on
    /// disk). Returns the parsed theme.
    @discardableResult
    func importTheme(from url: URL) throws -> TerminalTheme {
        let source: String
        do {
            source = try String(contentsOf: url, encoding: .utf8)
        } catch {
            throw ImportError.cannotRead(message: error.localizedDescription)
        }

        let filename = url.deletingPathExtension().lastPathComponent
        let id = Self.slug(from: filename)
        let displayName = Self.displayName(from: filename)

        let destination = supportDirectory
            .appendingPathComponent("\(id).ghostty", isDirectory: false)

        let theme: TerminalTheme
        do {
            theme = try GhosttyThemeParser.parse(
                source,
                id: id,
                displayName: displayName,
                url: destination
            )
        } catch let parseError as GhosttyThemeParser.ParseError {
            throw ImportError.parseFailed(parseError)
        }

        do {
            if FileManager.default.fileExists(atPath: destination.path) {
                try FileManager.default.removeItem(at: destination)
            }
            try source.write(to: destination, atomically: true, encoding: .utf8)
        } catch {
            throw ImportError.cannotPersist(message: error.localizedDescription)
        }

        imported.removeAll { $0.id == id }
        imported.append(theme)
        imported.sort { $0.displayName < $1.displayName }
        return theme
    }

    /// Remove an imported theme. Built-ins can't be removed — callers
    /// should gate the trash affordance on `theme.source == .builtIn`
    /// being false. No-op if the theme isn't in `imported`.
    func remove(_ theme: TerminalTheme) throws {
        guard case .imported(let url) = theme.source else { return }
        try? FileManager.default.removeItem(at: url)
        imported.removeAll { $0.id == theme.id }
    }

    // MARK: - Loading

    /// Re-reads every `.ghostty` / `.conf` file in `supportDirectory`.
    /// Files that fail to parse are dropped silently — we log in debug
    /// but don't want a single malformed file to block every valid one.
    func reloadImported() {
        let fm = FileManager.default
        guard
            let entries = try? fm.contentsOfDirectory(
                at: supportDirectory,
                includingPropertiesForKeys: nil,
                options: [.skipsHiddenFiles]
            )
        else {
            imported = []
            return
        }
        var parsed: [TerminalTheme] = []
        for url in entries {
            let ext = url.pathExtension.lowercased()
            guard ext == "ghostty" || ext == "conf" else { continue }
            guard let source = try? String(contentsOf: url, encoding: .utf8) else { continue }
            let filename = url.deletingPathExtension().lastPathComponent
            let id = Self.slug(from: filename)
            let displayName = Self.displayName(from: filename)
            if let theme = try? GhosttyThemeParser.parse(
                source, id: id, displayName: displayName, url: url
            ) {
                parsed.append(theme)
            }
        }
        parsed.sort { $0.displayName < $1.displayName }
        imported = parsed
    }

    // MARK: - Helpers

    /// "Catppuccin Frappe" / "catppuccin_frappe" / "catppuccin-frappe"
    /// all collapse to `"catppuccin-frappe"`. Keeps ids stable across
    /// the common filename conventions user theme packs ship with.
    static func slug(from name: String) -> String {
        let lowered = name.lowercased()
        var out = ""
        var lastWasHyphen = false
        for char in lowered {
            if char.isLetter || char.isNumber {
                out.append(char)
                lastWasHyphen = false
            } else if !lastWasHyphen, !out.isEmpty {
                out.append("-")
                lastWasHyphen = true
            }
        }
        while out.hasSuffix("-") { out.removeLast() }
        return out.isEmpty ? "imported" : out
    }

    /// Human-friendly name for the Settings picker. Underscores and
    /// hyphens become spaces; each word gets capitalized.
    static func displayName(from filename: String) -> String {
        let spaced = filename
            .replacingOccurrences(of: "_", with: " ")
            .replacingOccurrences(of: "-", with: " ")
        return spaced
            .split(separator: " ")
            .map { $0.prefix(1).uppercased() + $0.dropFirst() }
            .joined(separator: " ")
    }
}
