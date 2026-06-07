//
//  LivePaneRegistry.swift
//  Nice
//
//  Process-wide side channel for handing a *live* pane between windows.
//
//  A pane drag only carries the pane id on the pasteboard (see the pill
//  `.onDrag` in `WindowToolbarView`). A running pty + its
//  `NiceTerminalView` cannot ride the pasteboard, so when a pill is
//  dragged from one window's strip into another window — or off the
//  window entirely (tear-off) — the destination resolves that pasteboard
//  id against this registry to claim the live `TabPtySession.PaneEntry`
//  from the source window.
//
//  Lifecycle: the drag source `publish`es a `Handle` at drag start whose
//  `claim` closure detaches the entry from its source `SessionsModel`
//  on first call. The destination (cross-window drop or tear-off
//  controller) calls `claim(paneId:)` exactly once to take ownership.
//  Any drag that ends without a cross-window destination (an intra-
//  window reorder, or a cancelled drag) calls `withdraw` so the handle
//  is dropped without ever detaching — the pane stays put.
//
//  Owned by `NiceServices` (one instance per process) so every window's
//  pill and drop delegate reach the same channel.
//

import Foundation

@MainActor
@Observable
final class LivePaneRegistry {
    /// A claimable handle to a live pane being dragged.
    struct Handle {
        let paneId: String
        /// `windowSessionId` of the window the pane is being dragged
        /// FROM. The drop side compares this against its own window id
        /// to distinguish an intra-window reorder (same id → never
        /// claim) from a genuine cross-window move.
        let sourceWindowSessionId: String
        /// Tab the pane currently lives in, in the source window.
        let sourceTabId: String
        /// Detach the live entry from its source on the FIRST call and
        /// return it; later calls (or a pane that already exited) return
        /// nil. Supplied by the drag source so this type stays unaware
        /// of `SessionsModel`.
        let claim: () -> TabPtySession.PaneEntry?
    }

    /// Active drag handles keyed by pane id. At most one drag is in
    /// flight at a time in practice, but keying by pane id keeps the
    /// lookup unambiguous and lets a stray earlier handle be overwritten
    /// cleanly by a new `publish`.
    private var handles: [String: Handle] = [:]

    /// The single drag currently in flight (a mouse can only drag one
    /// thing at a time), or nil. Set on `publish`, cleared on `claim` /
    /// `withdraw`. Lets a destination window's drop delegate read the
    /// dragged pane's identity SYNCHRONOUSLY during hover — SwiftUI's
    /// `.onDrop` doesn't surface the pasteboard payload until the drop
    /// commits, and a foreign drag never populated this window's local
    /// `dragState`. The drop still verifies intent (the pasteboard
    /// carries the same pane id) before committing.
    private(set) var currentDrag: Handle?

    /// Register a live pane as draggable. Called by the drag source at
    /// drag start. Overwrites any prior handle for the same pane id.
    func publish(_ handle: Handle) {
        handles[handle.paneId] = handle
        currentDrag = handle
    }

    /// Look up (without claiming) the handle for `paneId`. Used by a
    /// drop delegate to paint the foreign-drag insertion indicator and
    /// to decide whether the drop is cross-window before committing.
    func handle(forPaneId id: String) -> Handle? {
        handles[id]
    }

    /// Take ownership of the live entry for `paneId`. One-shot: the
    /// handle is removed whether or not the detach succeeds, so a stray
    /// second drop can't double-migrate the same pane. Returns the
    /// handle (for its source context) plus the detached live entry, or
    /// nil when no handle is registered or the pane has already exited.
    func claim(paneId id: String) -> (handle: Handle, entry: TabPtySession.PaneEntry)? {
        if currentDrag?.paneId == id { currentDrag = nil }
        guard let handle = handles.removeValue(forKey: id) else { return nil }
        guard let entry = handle.claim() else { return nil }
        return (handle, entry)
    }

    /// Drop the handle for `paneId` without detaching. Called when a
    /// drag ends with no cross-window destination (intra-window reorder
    /// or cancel) so the published closure is never invoked.
    func withdraw(paneId id: String) {
        if currentDrag?.paneId == id { currentDrag = nil }
        handles.removeValue(forKey: id)
    }
}
