//
//  FileBrowserDropResolver.swift
//  Nice
//
//  Pure decision logic for the file-browser's drag-to-folder behaviour.
//  Mirrors `SidebarDropResolver`'s shape: AppKit / SwiftUI-free helpers
//  the row drop delegate calls. Splitting them out lets unit tests
//  exercise the rules — folder-into-self rejection, same-parent no-op,
//  Option-as-copy, cross-volume-as-copy — without standing up a live
//  drag session.
//

import AppKit
import Foundation

enum FileBrowserDropResolver {

    /// Whether a drop of `sources` into `dest` would do anything.
    /// Returns `false` for any of:
    ///   - empty `sources`,
    ///   - `dest` equals one of the sources,
    ///   - `dest` is a descendant of one of the sources (would form
    ///     a cycle), or
    ///   - every source already lives directly inside `dest`.
    /// Returns `true` as long as at least one source would actually
    /// move/copy and none of the cycle rules apply.
    ///
    /// Pure path-string logic — no filesystem reads. Callers are
    /// responsible for ensuring `dest` is an existing directory.
    static func canDrop(sources: [URL], into dest: URL) -> Bool {
        guard !sources.isEmpty else { return false }
        let destPath = dest.standardizedFileURL.path
        var anyWouldMove = false
        for src in sources {
            let srcPath = src.standardizedFileURL.path
            if destPath == srcPath { return false }
            if destPath.hasPrefix(srcPath + "/") { return false }
            let parent = src.deletingLastPathComponent().standardizedFileURL.path
            if parent != destPath { anyWouldMove = true }
        }
        return anyWouldMove
    }

    /// Resolve move-vs-copy for a drop. Matches Finder defaults:
    ///   - Option held → always copy,
    ///   - cross-volume → copy (move would fail across volumes
    ///     anyway in raw `FileManager.moveItem`, and Finder's UX is
    ///     to copy here),
    ///   - same-volume + no Option → move.
    /// Pure — `sameVolume` is hoisted to a parameter so the rule is
    /// testable without filesystem fixtures.
    static func operation(
        modifierFlags: NSEvent.ModifierFlags,
        sameVolume: Bool
    ) -> FileDragOperation {
        if modifierFlags.contains(.option) { return .copy }
        return sameVolume ? .move : .copy
    }

    /// Convenience wrapper that reads each URL's volume identifier
    /// via `URLResourceValues`. Touches the filesystem, so it lives
    /// outside the pure rule above. Returns `false` if either URL's
    /// volume id can't be read — defensive default that picks copy.
    static func areOnSameVolume(_ a: URL, _ b: URL) -> Bool {
        let aValues = try? a.resourceValues(forKeys: [.volumeIdentifierKey])
        let bValues = try? b.resourceValues(forKeys: [.volumeIdentifierKey])
        guard let aId = aValues?.volumeIdentifier as? NSObject,
              let bId = bValues?.volumeIdentifier as? NSObject
        else { return false }
        return aId.isEqual(bId)
    }
}
