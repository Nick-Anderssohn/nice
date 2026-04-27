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
//  Mirrors the `FontSettings` shape: an `@MainActor @Observable` class
//  whose mutations write through to an injectable `UserDefaults`
//  instance (so unit tests can stand it up against an isolated suite).
//
//  Folders-first is intentionally NOT a setting here — the file
//  browser always groups directories above files regardless of
//  criterion. Sort applies within each bucket.
//

import Foundation

@MainActor
@Observable
final class FileBrowserSortSettings {
    static let criterionKey = "fileBrowser.sort.criterion"
    static let ascendingKey = "fileBrowser.sort.ascending"

    /// Nested-spelling alias for the file-scope `FileBrowserSortCriterion`
    /// enum (defined in `FileBrowserListing.swift`). The enum lives
    /// next to the comparator that dispatches on it; this typealias
    /// keeps `FileBrowserSortSettings.Criterion` working for callers
    /// that prefer the namespaced form.
    typealias Criterion = FileBrowserSortCriterion

    var criterion: Criterion {
        didSet { defaults.set(criterion.rawValue, forKey: Self.criterionKey) }
    }

    /// `true` = ascending. For names, that means A→Z; for dates,
    /// oldest first. Direction is **independent** of criterion — the
    /// user toggles it explicitly via the breadcrumb's ↑/↓ button, so
    /// switching criterion does not silently flip "newest first" to
    /// "A→Z" or vice versa.
    var ascending: Bool {
        didSet { defaults.set(ascending, forKey: Self.ascendingKey) }
    }

    @ObservationIgnored
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
