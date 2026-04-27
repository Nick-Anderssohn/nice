//
//  FileBrowserSortSettings.swift
//  Nice
//
//  Process-wide sort preferences for the sidebar's file browser:
//  which criterion (name vs. modification date) and which direction
//  (ascending vs. descending). Persisted in `UserDefaults` so the
//  user's choice survives relaunches and is shared across every tab
//  and window — sort feels like a "view preference" akin to Finder's,
//  not a per-folder setting.
//
//  Mirrors the `FontSettings` shape: an `@MainActor ObservableObject`
//  whose mutations write through to an injectable `UserDefaults`
//  instance (so unit tests can stand it up against an isolated suite).
//
//  Folders-first is intentionally NOT a setting here — the file
//  browser always groups directories above files regardless of
//  criterion. Sort applies within each bucket.
//

import Foundation

@MainActor
final class FileBrowserSortSettings: ObservableObject {
    static let criterionKey = "fileBrowser.sort.criterion"
    static let ascendingKey = "fileBrowser.sort.ascending"

    /// Sort key applied within the dirs / files buckets.
    enum Criterion: String, CaseIterable {
        /// Case-insensitive lexicographic on the entry's last path
        /// component. Today's behavior, kept as the default so a
        /// fresh install reads identically to prior versions.
        case name
        /// `URLResourceValues.contentModificationDate`. Useful for
        /// "what did I touch most recently" workflows.
        case dateModified
    }

    @Published var criterion: Criterion {
        didSet { defaults.set(criterion.rawValue, forKey: Self.criterionKey) }
    }

    /// `true` = ascending. For names, that means A→Z; for dates,
    /// oldest first. Direction is **independent** of criterion — the
    /// user toggles it explicitly via the breadcrumb's ↑/↓ button, so
    /// switching criterion does not silently flip "newest first" to
    /// "A→Z" or vice versa.
    @Published var ascending: Bool {
        didSet { defaults.set(ascending, forKey: Self.ascendingKey) }
    }

    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        let rawCriterion = defaults.string(forKey: Self.criterionKey) ?? ""
        self.criterion = Criterion(rawValue: rawCriterion) ?? .name
        // `object(forKey:)` so an unset key falls back to ascending,
        // not to `bool(forKey:)`'s implicit `false`.
        if let stored = defaults.object(forKey: Self.ascendingKey) as? Bool {
            self.ascending = stored
        } else {
            self.ascending = true
        }
    }
}
