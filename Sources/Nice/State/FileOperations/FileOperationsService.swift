//
//  FileOperationsService.swift
//  Nice
//
//  Pure filesystem worker behind the file-browser context menu.
//  Knows how to copy, move, and trash file URLs, with Finder-style
//  collision auto-renaming (`foo copy.txt`, `foo copy 2.txt`, ...).
//  Returns a `FileOperation` record so the history layer can undo
//  without re-reading the filesystem.
//
//  No SwiftUI / AppState dependency: the worker takes URLs in and
//  returns records out. All async-ish work (notably `NSWorkspace
//  .recycle`) is wrapped behind a synchronous `try` so callers see
//  the same shape for every op type.
//
//  The class is final and `@MainActor`-isolated to match the rest
//  of the app's actor model. It holds no mutable state — instances
//  are interchangeable. We expose it as a class only so tests can
//  swap a fake `FileManager` and `Trasher` in via init injection.
//

import AppKit
import Foundation

/// Errors the service can surface. Drift errors are reported back to
/// the history layer, which decides whether to skip the offending
/// item and continue, or abort the inverse op.
enum FileOperationError: Error, Equatable {
    /// A source URL that should still exist is missing.
    case sourceMissing(URL)
    /// A trashed item we wanted to restore is gone (user emptied
    /// Trash, or moved it elsewhere).
    case trashedItemMissing(URL)
    /// Wrapped Foundation error with a human-readable description.
    case underlying(String)
}

/// Boundary protocol so unit tests can stub Trash without invoking
/// the real `FileManager.trashItem`. The default implementation
/// uses `FileManager.trashItem(at:resultingItemURL:)`, which is
/// synchronous on macOS and returns the resulting Trash URL via
/// an inout parameter.
protocol Trasher {
    /// Move `urls` to the user's Trash. Returns the new URLs in the
    /// Trash folder, in the same order as `urls`. Throws on the
    /// first failure (the items already moved up to that point are
    /// left in Trash — undo restores them piecemeal).
    func recycle(_ urls: [URL]) throws -> [URL]
}

struct FileManagerTrasher: Trasher {
    private let fileManager: FileManager

    init(fileManager: FileManager = .default) {
        self.fileManager = fileManager
    }

    func recycle(_ urls: [URL]) throws -> [URL] {
        var out: [URL] = []
        for url in urls {
            var resulting: NSURL?
            do {
                try fileManager.trashItem(at: url, resultingItemURL: &resulting)
            } catch {
                throw FileOperationError.underlying(error.localizedDescription)
            }
            guard let resulting = resulting as URL? else {
                throw FileOperationError.underlying(
                    "trashItem returned no resulting URL for \(url.path)"
                )
            }
            out.append(resulting)
        }
        return out
    }
}

@MainActor
final class FileOperationsService {
    private let fileManager: FileManager
    private let trasher: Trasher

    init(
        fileManager: FileManager = .default,
        trasher: Trasher? = nil
    ) {
        self.fileManager = fileManager
        self.trasher = trasher ?? FileManagerTrasher(fileManager: fileManager)
    }

    // MARK: - Copy / Move

    /// Copy each item in `items` into `dest`. Names that collide with
    /// existing entries get a `" copy"`, `" copy 2"`, ... suffix per
    /// `nextAvailableName`. Returns a `.copy` record describing every
    /// resulting source→dest pair.
    func copy(
        items: [URL],
        into dest: URL,
        origin: FileOperationOrigin
    ) throws -> FileOperation {
        let pairs = resolveDestinations(items: items, into: dest)
        return try apply(.copy(items: pairs, origin: origin))
    }

    /// Move each item in `items` into `dest`. Same collision policy
    /// as `copy`. Returns a `.move` record.
    func move(
        items: [URL],
        into dest: URL,
        origin: FileOperationOrigin
    ) throws -> FileOperation {
        let pairs = resolveDestinations(items: items, into: dest)
        return try apply(.move(items: pairs, origin: origin))
    }

    // MARK: - Trash

    /// Trash each item in `items`. Returns a `.trash` record carrying
    /// the resulting Trash URLs so undo can restore from them.
    func trash(
        items: [URL],
        origin: FileOperationOrigin
    ) throws -> FileOperation {
        // The trash variant of `apply` re-trashes from `original`s on
        // each invocation, so we seed a record whose `original`s are
        // the input URLs (the `trashed` field gets overwritten by the
        // first-time recycle inside apply).
        let seedItems = items.map { FileTrashItem(original: $0, trashed: $0) }
        return try apply(.trash(items: seedItems, origin: origin))
    }

    // MARK: - Apply / Undo (used by FileOperationHistory)

    /// Re-apply `op` exactly as it was first performed. Used by the
    /// history's `redo()`. For copy and move, the source/destination
    /// pairs are reused as-is; for trash, the originals are re-
    /// trashed and the resulting record carries the *new* Trash
    /// URLs (the system relocates each pass).
    func apply(_ op: FileOperation) throws -> FileOperation {
        switch op {
        case let .copy(items, origin):
            try forEachItem(items) { item in
                try fileManager.copyItem(at: item.source, to: item.destination)
            }
            return .copy(items: items, origin: origin)

        case let .move(items, origin):
            try forEachItem(items) { item in
                try fileManager.moveItem(at: item.source, to: item.destination)
            }
            return .move(items: items, origin: origin)

        case let .trash(items, origin):
            // Re-trash the originals and capture the new trash URLs
            // — the system gives each recycle a fresh location.
            let originals = items.map { $0.original }
            for url in originals { try checkExists(url) }
            let newTrashed = try trasher.recycle(originals)
            let newItems = zip(originals, newTrashed).map { o, t in
                FileTrashItem(original: o, trashed: t)
            }
            return .trash(items: newItems, origin: origin)
        }
    }

    /// Run `body` against each item, surfacing source-missing drift
    /// before the move/copy attempt and wrapping any Foundation
    /// failure as `.underlying`. Shared by `apply`'s copy + move
    /// branches so the loop bookkeeping lives in one place.
    private func forEachItem(
        _ items: [FileOperationItem],
        body: (FileOperationItem) throws -> Void
    ) throws {
        for item in items {
            try checkExists(item.source)
            do {
                try body(item)
            } catch let error as FileOperationError {
                throw error
            } catch {
                throw FileOperationError.underlying(error.localizedDescription)
            }
        }
    }

    /// Undo `op`. State is moved back to "before `op` was applied".
    /// Throws on drift (input file gone where we expected one); the
    /// history layer catches and reports the drift to the user.
    func undo(_ op: FileOperation) throws {
        switch op {
        case let .copy(items, _):
            // Inverse of a copy is to delete each destination. If
            // the destination is already gone (user manually
            // deleted it via Finder) the undo is silently
            // satisfied — the world already matches the desired
            // post-undo state for that item.
            for item in items {
                if fileManager.fileExists(atPath: item.destination.path) {
                    do {
                        try fileManager.removeItem(at: item.destination)
                    } catch {
                        throw FileOperationError.underlying(error.localizedDescription)
                    }
                }
            }

        case let .move(items, _):
            // Inverse of a move is to move dest → source. Drift on
            // the destination (file gone) is reported so the user
            // knows their undo couldn't complete.
            for item in items {
                try checkExists(item.destination)
                do {
                    try fileManager.moveItem(at: item.destination, to: item.source)
                } catch {
                    throw FileOperationError.underlying(error.localizedDescription)
                }
            }

        case let .trash(items, _):
            for item in items {
                guard fileManager.fileExists(atPath: item.trashed.path) else {
                    throw FileOperationError.trashedItemMissing(item.trashed)
                }
                do {
                    try fileManager.moveItem(at: item.trashed, to: item.original)
                } catch {
                    throw FileOperationError.underlying(error.localizedDescription)
                }
            }
        }
    }

    // MARK: - Collision naming

    /// Build the destination URLs for `items` inside `dest`. Each
    /// destination is collision-resolved via `nextAvailableName`,
    /// considering both existing filesystem entries and earlier
    /// pairs in the same batch (so two source files with the same
    /// name don't both pick `foo copy.txt`).
    private func resolveDestinations(
        items: [URL],
        into dest: URL
    ) -> [FileOperationItem] {
        var taken: Set<String> = []
        var out: [FileOperationItem] = []
        for src in items {
            let resolved = nextAvailableName(
                for: src,
                in: dest,
                additionalTaken: taken
            )
            taken.insert(resolved.lastPathComponent)
            out.append(FileOperationItem(source: src, destination: resolved))
        }
        return out
    }

    /// Return a destination URL for copying / moving `src` into
    /// `dest`. If `dest/<src.lastPathComponent>` is free, return it
    /// unchanged. Otherwise return `dest/<base> copy<.ext>`,
    /// `dest/<base> copy 2<.ext>`, ... incrementing until a free
    /// name is found. `additionalTaken` is consulted alongside the
    /// filesystem so a single batch with two same-named sources
    /// ends up with two distinct destinations.
    func nextAvailableName(
        for src: URL,
        in dest: URL,
        additionalTaken: Set<String> = []
    ) -> URL {
        let originalName = src.lastPathComponent
        let candidate = dest.appendingPathComponent(originalName)
        if !exists(candidate) && !additionalTaken.contains(originalName) {
            return candidate
        }

        let (base, ext) = Self.splitNameAndExtension(originalName)

        // Try " copy", " copy 2", " copy 3", ...
        var index = 1
        while true {
            let suffix = (index == 1) ? " copy" : " copy \(index)"
            let name = ext.isEmpty ? "\(base)\(suffix)" : "\(base)\(suffix).\(ext)"
            let url = dest.appendingPathComponent(name)
            if !exists(url) && !additionalTaken.contains(name) {
                return url
            }
            index += 1
            if index > 9999 {
                // Defensive backstop. Real filesystems never get
                // here; tests for pathological dirs would. Returning
                // a unique-by-UUID name keeps callers from looping.
                return dest.appendingPathComponent(
                    "\(base) copy \(UUID().uuidString)\(ext.isEmpty ? "" : ".\(ext)")"
                )
            }
        }
    }

    /// Split `"archive.tar.gz"` into `("archive.tar", "gz")`. We
    /// only treat the last extension as the extension; that matches
    /// Finder's behavior when it auto-renames.
    static func splitNameAndExtension(_ name: String) -> (base: String, ext: String) {
        // Names that *start* with a dot (`.zshrc`) are treated as
        // having no extension — the leading dot is part of the base
        // name, not a separator.
        if name.hasPrefix(".") {
            // For dotfiles like `.zshrc`, the whole name is base.
            // For `.zshrc.bak`, split at the last dot.
            let trimmed = String(name.dropFirst())
            guard let dotIdx = trimmed.lastIndex(of: ".") else {
                return (name, "")
            }
            let base = "." + trimmed[..<dotIdx]
            let ext = String(trimmed[trimmed.index(after: dotIdx)...])
            return (base, ext)
        }
        guard let dotIdx = name.lastIndex(of: ".") else {
            return (name, "")
        }
        let base = String(name[..<dotIdx])
        let ext = String(name[name.index(after: dotIdx)...])
        return (base, ext)
    }

    // MARK: - Helpers

    private func exists(_ url: URL) -> Bool {
        fileManager.fileExists(atPath: url.path)
    }

    private func checkExists(_ url: URL) throws {
        if !exists(url) {
            throw FileOperationError.sourceMissing(url)
        }
    }
}
