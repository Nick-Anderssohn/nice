//
//  FileOperation.swift
//  Nice
//
//  Record types for file operations the user has invoked from the
//  file-browser context menu (Copy/Paste, Cut/Paste, Move to Trash).
//  Each operation carries enough information to apply its inverse
//  without re-reading the filesystem — so undo stays fast and is
//  robust to the user moving things around between op and undo.
//
//  These types are pure value types with no SwiftUI / AppState
//  dependency, so the service layer that consumes them is fully
//  unit-testable in isolation.
//

import Foundation

/// Identifies which window/tab originated the op so undo/redo can
/// follow focus back to where the change happened. `tabId` is
/// optional — operations triggered from the keyboard outside of any
/// tab context still record a window.
struct FileOperationOrigin: Equatable, Sendable {
    let windowSessionId: String
    let tabId: String?
}

/// One source→destination pair from a Copy or Move op. Lives on
/// `FileOperation` rather than as a tuple so the encoding is named.
struct FileOperationItem: Equatable, Sendable {
    let source: URL
    let destination: URL
}

/// One trash record: where the file was, and where it went in Trash.
/// `trashed` is non-nil after a successful recycle; on undo we move
/// `trashed` back to `original`.
struct FileTrashItem: Equatable, Sendable {
    let original: URL
    let trashed: URL
}

/// Record of a completed file operation. The undo system flips one
/// of these into the inverse op, applies it, and pushes the inverse
/// onto the redo stack. `origin` is preserved so undo/redo can
/// route focus back to the originating tab.
enum FileOperation: Equatable, Sendable {
    case copy(items: [FileOperationItem], origin: FileOperationOrigin)
    case move(items: [FileOperationItem], origin: FileOperationOrigin)
    case trash(items: [FileTrashItem], origin: FileOperationOrigin)

    var origin: FileOperationOrigin {
        switch self {
        case let .copy(_, origin):  return origin
        case let .move(_, origin):  return origin
        case let .trash(_, origin): return origin
        }
    }

    /// Human-readable label for the transient drift / status banner.
    var label: String {
        switch self {
        case .copy:  return "Copy"
        case .move:  return "Move"
        case .trash: return "Move to Trash"
        }
    }
}
