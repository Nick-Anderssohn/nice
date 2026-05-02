//
//  PaneDragPayload.swift
//  Nice
//
//  Payload encoded into NSItemProvider / NSPasteboardItem for pane-pill
//  drags. Carries enough context for any window's drop target to:
//    1. Look up the source `AppState` via
//       `WindowRegistry.appState(forSessionId:)`.
//    2. Validate the Claude rule cheaply without resolving the source
//       (`kind` is duplicated from the model).
//    3. Resolve the source tab + pane and detach the pty view.
//
//  Uses a dedicated UTType so it doesn't collide with the sidebar
//  tab-reorder drag (which uses `.text` + a tab id string).
//

import Foundation
import UniformTypeIdentifiers

struct PaneDragPayload: Codable, Hashable, Sendable {
    let schema: Int
    let windowSessionId: String
    let tabId: String
    let paneId: String
    let kind: PaneKind

    static let currentSchema = 1
    static let utTypeIdentifier = "dev.nickanderssohn.nice.pane"

    /// Lazily-constructed UTType for `.onDrop(of:)` filters. Falls back
    /// to a runtime-exported type so the value is always non-nil even
    /// if the `Info.plist` declaration is missing — the pasteboard
    /// routes by identifier string regardless.
    static let utType: UTType = {
        UTType(utTypeIdentifier)
            ?? UTType(exportedAs: utTypeIdentifier, conformingTo: .data)
    }()

    init(windowSessionId: String, tabId: String, paneId: String, kind: PaneKind) {
        self.schema = Self.currentSchema
        self.windowSessionId = windowSessionId
        self.tabId = tabId
        self.paneId = paneId
        self.kind = kind
    }

    /// JSON-encode for stuffing into a pasteboard item. The encoding
    /// is stable and version-tagged so future changes can be migrated
    /// without breaking in-flight drags.
    func encoded() -> Data {
        // JSONEncoder with no special options is deterministic for this
        // small struct of primitives — fine to force-try.
        return (try? JSONEncoder().encode(self)) ?? Data()
    }

    static func decode(from data: Data) -> PaneDragPayload? {
        try? JSONDecoder().decode(PaneDragPayload.self, from: data)
    }
}
