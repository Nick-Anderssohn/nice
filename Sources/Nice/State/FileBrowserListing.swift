//
//  FileBrowserListing.swift
//  Nice
//
//  Pure directory-listing logic for the file browser: read a
//  directory's children, filter dotfiles by `showHidden`, and sort
//  dirs-first then by the user's chosen criterion + direction. Lifted
//  out of `FileTreeRow.reloadChildren` so the filter / sort / fallback
//  semantics can be tested directly against a real temp directory
//  without spinning up a SwiftUI view.
//
//  The function is `static` and side-effect-free: it reads the
//  filesystem and returns. Callers (today: `FileTreeRow` and the
//  Shift-range selection helper) are responsible for assigning the
//  result into their @State.
//

import Foundation

/// Sort key applied within the dirs / files buckets of a directory
/// listing. Lives at file scope (not nested inside
/// `FileBrowserSortSettings`) so the pure listing module doesn't have
/// to reach into the settings type just to name the enum it
/// dispatches on. `FileBrowserSortSettings` re-exposes it as a
/// nested `Criterion` typealias for callers that prefer the
/// namespaced spelling.
enum FileBrowserSortCriterion: String, CaseIterable {
    /// Case-insensitive lexicographic on the entry's last path
    /// component. The default for fresh installs, matches pre-sort
    /// versions of the file browser.
    case name
    /// `URLResourceValues.contentModificationDate`. Useful for "what
    /// did I touch most recently" workflows.
    case dateModified
}

enum FileBrowserListing {
    /// Read `url`'s children, optionally filtering hidden entries,
    /// and return them sorted dirs-first then by `criterion` /
    /// `ascending`.
    ///
    /// - Filtering: when `showHidden == false`, drops entries whose
    ///   name starts with `.` OR whose `URLResourceValues.isHidden`
    ///   flag is set. The dual check covers two distinct hidden
    ///   conventions: dotfiles by name, and Finder-flagged invisible
    ///   files via `chflags hidden`.
    ///
    /// - Sort: directories before files (always, regardless of
    ///   `criterion`); within each bucket, the chosen criterion
    ///   decides order. `ascending = true` means A→Z for names and
    ///   oldest-first for dates. Date sorts tie-break by name so the
    ///   ordering is stable when timestamps match (common after a
    ///   `git checkout`).
    ///
    /// - Errors / missing paths return `[]` rather than throwing —
    ///   the file browser's empty-state UI handles a missing root
    ///   path independently, and a deeper row that vanishes mid-
    ///   render shouldn't take the whole tree down.
    static func entries(
        at url: URL,
        showHidden: Bool,
        criterion: FileBrowserSortCriterion = .name,
        ascending: Bool = true
    ) -> [URL] {
        let fm = FileManager.default
        let keys: [URLResourceKey] = [
            .isDirectoryKey, .isHiddenKey, .nameKey,
            .contentModificationDateKey,
        ]
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
            return compareWithinBucket(
                lhs, rhs, criterion: criterion, ascending: ascending
            )
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
        showHidden: Bool,
        criterion: FileBrowserSortCriterion = .name,
        ascending: Bool = true
    ) -> [String] {
        let rootURL = URL(fileURLWithPath: rootPath)
        guard FileManager.default.fileExists(atPath: rootPath) else { return [] }
        var out: [String] = []
        visit(
            rootURL,
            into: &out,
            expandedPaths: expandedPaths,
            showHidden: showHidden,
            criterion: criterion,
            ascending: ascending
        )
        return out
    }

    private static func visit(
        _ url: URL,
        into out: inout [String],
        expandedPaths: Set<String>,
        showHidden: Bool,
        criterion: FileBrowserSortCriterion,
        ascending: Bool
    ) {
        out.append(url.path)
        guard expandedPaths.contains(url.path) else { return }
        let isDir = (try? url.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) ?? false
        guard isDir else { return }
        let kids = entries(
            at: url,
            showHidden: showHidden,
            criterion: criterion,
            ascending: ascending
        )
        for child in kids {
            visit(
                child,
                into: &out,
                expandedPaths: expandedPaths,
                showHidden: showHidden,
                criterion: criterion,
                ascending: ascending
            )
        }
    }

    /// Within-bucket comparator. Dirs-first grouping is enforced by
    /// the caller, so this only resolves order for two entries that
    /// are both files or both directories.
    private static func compareWithinBucket(
        _ lhs: URL,
        _ rhs: URL,
        criterion: FileBrowserSortCriterion,
        ascending: Bool
    ) -> Bool {
        switch criterion {
        case .name:
            let result = lhs.lastPathComponent
                .localizedCaseInsensitiveCompare(rhs.lastPathComponent)
            return ascending ? result == .orderedAscending : result == .orderedDescending
        case .dateModified:
            // Default missing-date entries to `.distantPast` so they
            // cluster deterministically at one end (the "oldest")
            // rather than scattering by whatever order the FS chose.
            let lhsDate = modificationDate(lhs) ?? .distantPast
            let rhsDate = modificationDate(rhs) ?? .distantPast
            if lhsDate != rhsDate {
                return ascending ? lhsDate < rhsDate : lhsDate > rhsDate
            }
            // Stable tie-break by name (always A→Z, so two ties don't
            // flip when the user toggles direction).
            return lhs.lastPathComponent
                .localizedCaseInsensitiveCompare(rhs.lastPathComponent) == .orderedAscending
        }
    }

    private static func modificationDate(_ url: URL) -> Date? {
        try? url.resourceValues(forKeys: [.contentModificationDateKey])
            .contentModificationDate
    }
}
