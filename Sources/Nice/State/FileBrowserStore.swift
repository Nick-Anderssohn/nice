//
//  FileBrowserStore.swift
//  Nice
//
//  Per-window catalog of `FileBrowserState` keyed by `Tab.id`. Owns
//  the lifecycle of the file-browser's per-tab state — lazy creation
//  on first access, removal on tab close, and the "toggle hidden
//  files iff a state already exists" semantics that the keyboard
//  shortcut needs.
//
//  Lives outside `AppState` so the file-browser feature has a
//  cohesive home: `AppState` owns the projects/tabs model and the
//  sidebar mode flag; the store owns the tab→browser-state map.
//  `AppState` keeps a reference to its window's store and forwards
//  cleanup (in `finalizeDissolvedTab`) and orchestration calls (the
//  ⌘⇧. shortcut handler that gates on `sidebarMode == .files`).
//

import Foundation
import SwiftUI

@MainActor
@Observable
final class FileBrowserStore {
    /// Keyed by `Tab.id`. Lazily populated by `ensureState` on first
    /// access, dropped by `removeState` when the tab closes. In-memory
    /// only — see `FileBrowserState` for why expansion / scroll
    /// state isn't worth persisting across launches.
    private(set) var states: [String: FileBrowserState] = [:]

    /// Fetch the state for `tabId`, creating one rooted at `cwd` if
    /// none exists. Subsequent calls return the same object so the
    /// view sees in-place mutations.
    func ensureState(forTab tabId: String, cwd: String) -> FileBrowserState {
        if let existing = states[tabId] { return existing }
        let state = FileBrowserState(rootPath: cwd)
        states[tabId] = state
        return state
    }

    /// Remove the state for a tab. Called by `AppState` when a tab
    /// is dissolved so the dictionary doesn't grow over the session.
    func removeState(forTab tabId: String) {
        states.removeValue(forKey: tabId)
    }

    /// Toggle hidden-file visibility for a tab IFF a state already
    /// exists. Returns `true` if a toggle happened. The "if exists"
    /// guard is what makes the ⌘⇧. shortcut a true no-op when the
    /// user has never opened the file browser for the active tab —
    /// no allocation, no published change for a feature they aren't
    /// looking at.
    @discardableResult
    func toggleHiddenFilesIfExists(forTab tabId: String) -> Bool {
        guard let state = states[tabId] else { return false }
        state.showHidden.toggle()
        return true
    }
}
