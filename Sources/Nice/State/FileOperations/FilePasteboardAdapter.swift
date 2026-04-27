//
//  FilePasteboardAdapter.swift
//  Nice
//
//  Adapter around `NSPasteboard.general` for the file-browser context
//  menu's Copy / Cut / Paste flow. Reads and writes file URLs using
//  the standard `public.file-url` type so Nice interoperates with
//  Finder both ways: Copy in Nice → Paste in Finder works, and Copy
//  in Finder → Paste in Nice works.
//
//  macOS has no native pasteboard concept of "cut files". We carry
//  the cut intent in an in-process companion struct keyed by the
//  pasteboard's `changeCount`. If anything (Nice or another app)
//  bumps the change count, the cut intent is invalidated and the
//  next read sees a plain copy. That matches what users expect:
//  cutting from Nice and copying something else in another app
//  before pasting acts as a copy of the latest pasteboard contents.
//
//  Tests inject an isolated `NSPasteboard(name:)` so the suite
//  doesn't read or scribble over the user's real clipboard.
//

import AppKit
import Foundation

@MainActor
@Observable
final class FilePasteboardAdapter {
    enum Intent: Equatable, Sendable {
        case copy
        case cut
    }

    /// Result of a successful read.
    struct Read: Equatable, Sendable {
        let urls: [URL]
        let intent: Intent
    }

    @ObservationIgnored
    private let pasteboard: NSPasteboard

    /// Companion record stamped on every `write(.cut, ...)`. Kept
    /// alongside the `changeCount` so any unrelated mutation of the
    /// pasteboard invalidates the cut.
    private struct CutCompanion: Equatable {
        let changeCount: Int
        let urls: [URL]
    }
    private var cutCompanion: CutCompanion?

    /// Track of the pasteboard's `changeCount` immediately after our
    /// last `write` call. Lets observers tell whether our copy intent
    /// is still the latest content vs. someone else has copied
    /// something else since.
    private(set) var lastWrittenChangeCount: Int?

    init(pasteboard: NSPasteboard = .general) {
        self.pasteboard = pasteboard
    }

    // MARK: - Read

    /// Read the current pasteboard contents as a list of file URLs.
    /// Returns `nil` if the pasteboard holds no `public.file-url`
    /// items. Cut intent is reported only when the in-process
    /// companion's change count matches the live pasteboard's.
    func read() -> Read? {
        let urls = pasteboard.readObjects(forClasses: [NSURL.self], options: nil)
            .flatMap { $0 as? [URL] } ?? []
        let fileURLs = urls.filter { $0.isFileURL }
        guard !fileURLs.isEmpty else { return nil }

        let intent: Intent = {
            guard let companion = cutCompanion,
                  companion.changeCount == pasteboard.changeCount,
                  companion.urls == fileURLs else {
                return .copy
            }
            return .cut
        }()
        return Read(urls: fileURLs, intent: intent)
    }

    // MARK: - Write

    /// Replace the pasteboard contents with `urls`. For `.cut`
    /// intent, also stamp the in-process companion so the next
    /// in-process `read()` sees `.cut`. External pasters always see
    /// the URLs as copies (no native cut concept on macOS).
    func write(urls: [URL], intent: Intent) {
        pasteboard.clearContents()
        // `NSPasteboard.writeObjects` accepts NSURL, which conforms
        // to NSPasteboardWriting. Nicely round-trips through
        // `readObjects(forClasses: [NSURL.self], ...)`.
        let nsURLs: [NSURL] = urls.map { $0 as NSURL }
        pasteboard.writeObjects(nsURLs)
        let count = pasteboard.changeCount
        lastWrittenChangeCount = count
        switch intent {
        case .copy:
            cutCompanion = nil
        case .cut:
            cutCompanion = CutCompanion(changeCount: count, urls: urls)
        }
    }

    /// Replace the pasteboard contents with plain text. Used by
    /// "Copy Path" so the same adapter owns every NSPasteboard
    /// mutation in the file-browser flow. Clears any cut companion
    /// since the previous file URLs are no longer current.
    func writeText(_ string: String) {
        pasteboard.clearContents()
        pasteboard.setString(string, forType: .string)
        cutCompanion = nil
        lastWrittenChangeCount = pasteboard.changeCount
    }

    /// Forget any cut companion so the next `read()` reports `.copy`.
    /// Called after a paste-from-cut completes — the source files
    /// have been moved, so the cut highlight in the UI must clear.
    func clearCutIntent() {
        cutCompanion = nil
    }

    /// True iff the in-process cut companion is still pointed at
    /// the live pasteboard. The view layer uses this to render the
    /// faded "ghost" treatment on rows whose paths are in the cut
    /// set.
    func isCut(_ url: URL) -> Bool {
        guard let companion = cutCompanion,
              companion.changeCount == pasteboard.changeCount else {
            return false
        }
        return companion.urls.contains(url)
    }

    /// Snapshot of paths currently in the cut companion (or empty
    /// if cut intent isn't current). Reads `cutCompanion` so the
    /// `@Observable` macro registers the dependency for re-renders.
    var cutPaths: Set<URL> {
        guard let companion = cutCompanion,
              companion.changeCount == pasteboard.changeCount else {
            return []
        }
        return Set(companion.urls)
    }
}
