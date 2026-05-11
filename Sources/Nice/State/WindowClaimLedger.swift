//
//  WindowClaimLedger.swift
//  Nice
//
//  Per-process set of `windowSessionId`s that some live `AppState` has
//  already adopted in `WindowSession.restoreSavedWindow`. Drives two
//  decisions:
//
//   1. `restoreSavedWindow` consults it to decide whether a fresh
//      window with no matching saved entry should adopt an unclaimed
//      saved slot (legitimate first-launch migration) or stay fresh
//      (⌘N opened a second window after the first already adopted
//      every available slot).
//
//   2. `WindowSession.unclaimedSavedWindowCount` reads it to tell the
//      launch-time fan-out in `AppShellHost.task` how many sibling
//      windows to spawn via `openWindow(id: "main")`.
//
//  Why a class (rather than the prior process-wide `static var`):
//  injection. Each unit test constructs its own ledger and threads it
//  into every `WindowSession` under test, so two tests running back-
//  to-back can't observe each other's claims. Production wires a
//  single instance on `NiceServices` and threads it into every
//  `AppState` it spawns, so claim semantics match the old static
//  exactly. The `_testing_resetClaimedWindowIds` / `_testing_isClaimed`
//  escape hatches are deleted as a result — tests now read directly
//  off the ledger they own.
//

import Foundation

@MainActor
final class WindowClaimLedger {
    private var claimedIds: Set<String> = []

    /// Mark this id as adopted by a live `WindowSession`. Called from
    /// the tail of `WindowSession.restoreSavedWindow`'s `defer` block
    /// so the claim lands even on the no-snapshot bail-out path
    /// (which would otherwise let a sibling window adopt the same
    /// slot and duplicate pane ids).
    func insert(_ id: String) {
        claimedIds.insert(id)
    }

    /// Release a previously-claimed id. Called from
    /// `WindowSession.tearDown(reason:)` regardless of reason — a
    /// future window in the same process must be free to adopt the
    /// (now-vacated) slot. For `.userClosedWindow` the slot is also
    /// gone from `sessions.json`, but that's the store's concern;
    /// the ledger only tracks who has it *right now*.
    func remove(_ id: String) {
        claimedIds.remove(id)
    }

    /// Whether any live `AppState` in this process is currently
    /// holding the slot.
    func contains(_ id: String) -> Bool {
        claimedIds.contains(id)
    }
}
