//
//  FakeSessionStore.swift
//  NiceUnitTests
//
//  Captures upsert / prune / flush calls so `WindowSession` tests can
//  exercise the persistence-side branches (save-gate,
//  restoreSavedWindow, tearDown's claimedWindowIds invariant) without
//  reading or writing `~/Library/Application Support/Nice/sessions.json`.
//
//  Mirrors the in-memory shape of the real `SessionStore`: `upsert`
//  replaces by id, `pruneEmptyWindows` drops empties except the kept
//  slot. That parity matters — `WindowSession.restoreSavedWindow` reads
//  `load()` and then calls `pruneEmptyWindows(keeping:)`, and tests
//  that assert pruning behavior need the fake to mutate `state` the
//  same way the real store does.
//

import Foundation
@testable import Nice

@MainActor
final class FakeSessionStore: SessionStorePersisting {
    /// Returned by `load()` and mutated by `upsert` / `pruneEmptyWindows`
    /// so successive reads see the most recent state. Tests preload
    /// this to control what `restoreSavedWindow` sees on entry.
    var state: PersistedState = .empty

    private(set) var upsertCalls: [PersistedWindow] = []
    private(set) var pruneKeepingCalls: [String] = []
    private(set) var flushCount = 0

    init(state: PersistedState = .empty) {
        self.state = state
    }

    func load() -> PersistedState { state }

    func upsert(window: PersistedWindow) {
        upsertCalls.append(window)
        var windows = state.windows.filter { $0.id != window.id }
        windows.append(window)
        state = PersistedState(version: PersistedState.currentVersion, windows: windows)
    }

    func pruneEmptyWindows(keeping: String) {
        pruneKeepingCalls.append(keeping)
        let filtered = state.windows.filter { $0.id == keeping || $0.totalTabCount > 0 }
        state = PersistedState(version: PersistedState.currentVersion, windows: filtered)
    }

    func flush() { flushCount += 1 }
}
