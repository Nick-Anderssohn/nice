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
//  `claim` closure resolves the source pane to a `PaneClaim` on first
//  call — `.live(entry)` (the pty was running; the entry is detached
//  one-shot), `.notSpawned(cwd:)` (modelled but spawn deferred; the
//  destination spawns it fresh), or `.gone`. The destination
//  (cross-window drop or tear-off controller) calls `claim(paneId:)`
//  exactly once to take ownership and switches over the tri-state.
//  Any drag that ends without a cross-window destination (an intra-
//  window reorder, or a cancelled drag) calls `withdraw` so the handle
//  is dropped without ever resolving — the pane stays put.
//
//  Owned by `NiceServices` (one instance per process) so every window's
//  pill and drop delegate reach the same channel.
//

import Foundation

/// The outcome of taking ownership of a dragged pane from its source
/// window. A CLOSED tri-state so every consumer (tear-off, migration)
/// must handle the unspawned case explicitly — the compiler refuses a
/// non-exhaustive switch, which is exactly the silent-nil trap (BUG A)
/// this type replaces:
///
///   - `.live(entry)`     — the pane had a running pty; the live
///     `PaneEntry` was detached one-shot and is handed over to migrate.
///   - `.notSpawned(cwd:)` — the pane is MODELLED in the source tab but
///     its pty spawn was still deferred (e.g. a terminal restored at
///     startup, never focused). There is nothing to migrate live; the
///     destination must spawn it fresh, in `cwd` (resolved from the
///     SOURCE model at claim time so a restored pane tears off into the
///     right directory).
///   - `.gone`            — the pane is neither live nor modelled
///     (already exited / withdrawn). Consumers abort.
enum PaneClaim {
    /// A live pty entry detached from the source session.
    case live(TabPtySession.PaneEntry)
    /// The pane exists in the model but was never spawned; carry the
    /// resolved spawn cwd so the destination can start it in place.
    case notSpawned(cwd: String)
    /// The pane is gone — no live entry and no model. Abort.
    case gone
}

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
        /// Resolve the source pane to a `PaneClaim` on the FIRST call:
        /// `.live(entry)` when the pty was running (the entry is
        /// detached one-shot), `.notSpawned(cwd:)` when the pane is
        /// modelled but its spawn was deferred, or `.gone` when neither
        /// holds. Supplied by the drag source so this type stays unaware
        /// of `SessionsModel`. The tri-state is what lets the tear-off /
        /// migration consumers spawn-and-proceed for a deferred pane
        /// instead of silently no-op'ing on it.
        let claim: () -> PaneClaim
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

    /// Take ownership of the pane for `paneId`. One-shot: the handle is
    /// removed whether the claim is `.live`, `.notSpawned`, or `.gone`,
    /// so a stray second drop can't double-claim the same pane. Returns
    /// the handle (for its source context) paired with the resolved
    /// `PaneClaim`.
    ///
    /// Returns nil ONLY when no handle is registered for `id` — the
    /// "already withdrawn / never published" case. When a handle IS
    /// registered the tuple is ALWAYS returned, even for `.gone`, so
    /// removal stays one-shot and the caller can distinguish
    /// `.notSpawned` (spawn-and-proceed) from `.gone` (abort). This is
    /// the structural fix for BUG A: the old API folded "not spawned
    /// yet" and "already dead" into a single nil that the consumers'
    /// `guard let` swallowed, silently dropping the tear-off / migration.
    func claim(paneId id: String) -> (handle: Handle, claim: PaneClaim)? {
        if currentDrag?.paneId == id { currentDrag = nil }
        guard let handle = handles.removeValue(forKey: id) else { return nil }
        return (handle, handle.claim())
    }

    /// Drop the handle for `paneId` without detaching. Called when a
    /// drag ends with no cross-window destination (intra-window reorder
    /// or cancel) so the published closure is never invoked.
    func withdraw(paneId id: String) {
        if currentDrag?.paneId == id { currentDrag = nil }
        handles.removeValue(forKey: id)
    }
}
