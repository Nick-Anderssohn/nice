//
//  FileBrowserRenameValidator.swift
//  Nice
//
//  Pure validator for the inline rename feature on the file-browser
//  sidebar. Lives outside the SwiftUI view so the rules are
//  unit-testable without standing up a row.
//
//  Three responsibilities:
//    • `canRename(_:)` — cheap pre-flight gate used by all three
//      triggers (slow-second-click, Return key, context-menu Rename)
//      to suppress entering edit mode for the filesystem root `/`.
//      `/` has no parent, so a rename can never produce a valid
//      destination.
//    • `validate(originalURL:draft:fileManager:)` — checks the user's
//      typed draft against the same illegal-name rules Finder uses,
//      plus a sibling-collision pre-flight so the row can surface a
//      drift message instead of letting `apply(.move)` throw.
//    • `isExtensionChange(originalName:newName:)` — yes/no on whether
//      the rename changes the file's extension, so the row can
//      present the Finder-style confirmation alert before committing.
//
//  No `FileManager` ownership: the static `validate` takes one as a
//  parameter so tests inject a fake. `canRename` and
//  `isExtensionChange` are pure-string predicates.
//

import Foundation

/// Outcome of evaluating a rename draft. The row maps each case to a
/// concrete commit/cancel/keep-editing action.
enum RenameValidation: Equatable {
    /// Draft is fine; commit to this destination URL.
    case ok(URL)
    /// Empty / whitespace-only draft. Cancel back to original.
    case empty
    /// Draft equals the original lastPathComponent. Cancel silently
    /// (no-op rename).
    case unchanged
    /// Draft contains `/` or `:` — both illegal in a single path
    /// component on macOS. Stay in edit mode so the user fixes it.
    case containsSlash
    /// A sibling at the parent already has this name. The destination
    /// URL is included so the row can quote the offending name in
    /// the drift message.
    case wouldCollide(URL)
    /// `originalURL.path == "/"`. Defense in depth — the trigger
    /// gates already block opening the field for `/`.
    case isFilesystemRoot
}

enum FileBrowserRenameValidator {

    /// Cheap pre-flight gate consulted by the trigger paths. Returns
    /// `false` for the filesystem root `/`, `true` everywhere else.
    /// The context menu uses this to hide the "Rename" entry; the row
    /// uses it to short-circuit `beginRename` before flipping into
    /// edit mode.
    static func canRename(_ url: URL) -> Bool {
        // `URL.path` strips a trailing slash, so `/` and `file:///`
        // both normalize to "/". Comparing the raw path is enough.
        url.path != "/"
    }

    /// Evaluate `draft` against `originalURL`. `fileManager` is used
    /// only for the sibling-collision check; it's parameterized so
    /// unit tests can inject a fake against a temp directory.
    static func validate(
        originalURL: URL,
        draft: String,
        fileManager: FileManager = .default
    ) -> RenameValidation {
        if !canRename(originalURL) { return .isFilesystemRoot }

        let trimmed = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty { return .empty }
        if trimmed == originalURL.lastPathComponent { return .unchanged }
        // Slash separates path components and `:` is the legacy HFS
        // separator; both Finder and POSIX reject either inside a
        // single name. We treat slash as the canonical "stay in edit
        // mode" signal — we don't want to silently treat the input as
        // a path.
        if trimmed.contains("/") || trimmed.contains(":") { return .containsSlash }

        let parent = originalURL.deletingLastPathComponent()
        let candidate = parent.appendingPathComponent(trimmed)
        if fileManager.fileExists(atPath: candidate.path) {
            return .wouldCollide(candidate)
        }
        return .ok(candidate)
    }

    /// True iff `originalName` and `newName` differ in extension.
    /// Defers to `FileOperationsService.splitNameAndExtension`, which
    /// already correctly handles dotfiles like `.zshrc` (whole name
    /// is base) and `.zshrc.bak` (split at the last dot). Pinning
    /// behaviour for those cases:
    ///   "foo.txt" ↔ "foo.md"     → true  (extension changed)
    ///   "foo.txt" ↔ "bar.txt"    → false (basename only)
    ///   ".zshrc"  ↔ ".zshrc.bak" → true  (gained a `.bak` extension)
    ///   "foo.txt" ↔ "foo"        → true  (extension removed)
    ///   "foo"     ↔ "foo.txt"    → true  (extension added)
    static func isExtensionChange(originalName: String, newName: String) -> Bool {
        let (_, oldExt) = FileOperationsService.splitNameAndExtension(originalName)
        let (_, newExt) = FileOperationsService.splitNameAndExtension(newName)
        return oldExt != newExt
    }
}
