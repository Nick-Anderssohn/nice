//
//  FileBrowserListing.swift
//  Nice
//
//  Pure directory-listing logic for the file browser: read a
//  directory's children, filter dotfiles by `showHidden`, and sort
//  dirs-first then alphabetical (case-insensitive). Lifted out of
//  `FileTreeRow.reloadChildren` so the filter / sort / fallback
//  semantics can be tested directly against a real temp directory
//  without spinning up a SwiftUI view.
//
//  The function is `static` and side-effect-free: it reads the
//  filesystem and returns. Callers (today: only `FileTreeRow`) are
//  responsible for assigning the result into their @State.
//

import Foundation

enum FileBrowserListing {
    /// Read `url`'s children, optionally filtering hidden entries,
    /// and return them sorted dirs-first then case-insensitive
    /// alphabetical — the look the file browser presents to the
    /// user.
    ///
    /// - Filtering: when `showHidden == false`, drops entries whose
    ///   name starts with `.` OR whose `URLResourceValues.isHidden`
    ///   flag is set. The dual check covers two distinct hidden
    ///   conventions: dotfiles by name, and Finder-flagged invisible
    ///   files via `chflags hidden`.
    ///
    /// - Sort: directories before files; within each bucket,
    ///   `localizedCaseInsensitiveCompare` so "M_dir" precedes
    ///   "Z_dir" and "a_file" precedes "b_file" regardless of case.
    ///
    /// - Errors / missing paths return `[]` rather than throwing —
    ///   the file browser's empty-state UI handles a missing root
    ///   path independently, and a deeper row that vanishes mid-
    ///   render shouldn't take the whole tree down.
    static func entries(at url: URL, showHidden: Bool) -> [URL] {
        let fm = FileManager.default
        let keys: [URLResourceKey] = [.isDirectoryKey, .isHiddenKey, .nameKey]
        guard let raw = try? fm.contentsOfDirectory(
            at: url,
            includingPropertiesForKeys: keys,
            options: [.skipsPackageDescendants]
        ) else {
            return []
        }

        let filtered: [URL]
        if showHidden {
            filtered = raw
        } else {
            filtered = raw.filter { childURL in
                let hidden = (try? childURL.resourceValues(forKeys: [.isHiddenKey]).isHidden) ?? false
                let dotPrefix = childURL.lastPathComponent.hasPrefix(".")
                return !hidden && !dotPrefix
            }
        }

        return filtered.sorted { lhs, rhs in
            let lhsDir = (try? lhs.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) ?? false
            let rhsDir = (try? rhs.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) ?? false
            if lhsDir != rhsDir { return lhsDir }
            return lhs.lastPathComponent
                .localizedCaseInsensitiveCompare(rhs.lastPathComponent) == .orderedAscending
        }
    }

    /// In-order traversal of the visible rows in the tree rooted at
    /// `rootPath`, using the same listing rules `entries` produces.
    /// A directory's children are emitted only when its path is in
    /// `expandedPaths` — matches what the user actually sees on
    /// screen, so it's the right ordering for Shift-range selection.
    static func visibleOrder(
        rootPath: String,
        expandedPaths: Set<String>,
        showHidden: Bool
    ) -> [String] {
        let rootURL = URL(fileURLWithPath: rootPath)
        guard FileManager.default.fileExists(atPath: rootPath) else { return [] }
        var out: [String] = []
        visit(rootURL, into: &out, expandedPaths: expandedPaths, showHidden: showHidden)
        return out
    }

    private static func visit(
        _ url: URL,
        into out: inout [String],
        expandedPaths: Set<String>,
        showHidden: Bool
    ) {
        out.append(url.path)
        guard expandedPaths.contains(url.path) else { return }
        let isDir = (try? url.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) ?? false
        guard isDir else { return }
        for child in entries(at: url, showHidden: showHidden) {
            visit(child, into: &out, expandedPaths: expandedPaths, showHidden: showHidden)
        }
    }
}
