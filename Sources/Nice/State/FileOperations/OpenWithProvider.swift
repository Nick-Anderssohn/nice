//
//  OpenWithProvider.swift
//  Nice
//
//  Wraps Launch Services to enumerate the apps that can open a given
//  file URL, plus the user's default app for that URL. Used by the
//  file-browser context menu's `Open With ▸` submenu.
//
//  The lookup is parameterised behind two closures so unit tests can
//  bypass the user's installed apps and exercise the ordering /
//  de-dup logic with a deterministic fixture list.
//

import AppKit
import Foundation

struct OpenWithEntry: Equatable {
    let appURL: URL
    let displayName: String
    let icon: NSImage?
    let isDefault: Bool

    static func == (lhs: OpenWithEntry, rhs: OpenWithEntry) -> Bool {
        lhs.appURL == rhs.appURL
            && lhs.displayName == rhs.displayName
            && lhs.isDefault == rhs.isDefault
    }
}

@MainActor
struct OpenWithProvider {
    /// Lookup callbacks. The defaults call into `NSWorkspace`; tests
    /// pass deterministic stubs.
    struct Lookups {
        var allAppsForURL: (URL) -> [URL]
        var defaultAppForURL: (URL) -> URL?
        var displayName: (URL) -> String
        var icon: (URL) -> NSImage?

        static var system: Lookups {
            Lookups(
                allAppsForURL: { url in
                    NSWorkspace.shared.urlsForApplications(toOpen: url)
                },
                defaultAppForURL: { url in
                    NSWorkspace.shared.urlForApplication(toOpen: url)
                },
                displayName: { url in
                    let bundle = Bundle(url: url)
                    let info = bundle?.infoDictionary
                    if let name = info?["CFBundleDisplayName"] as? String,
                       !name.isEmpty { return name }
                    if let name = info?["CFBundleName"] as? String,
                       !name.isEmpty { return name }
                    return url.deletingPathExtension().lastPathComponent
                },
                icon: { url in
                    NSWorkspace.shared.icon(forFile: url.path)
                }
            )
        }
    }

    private let lookups: Lookups

    init(lookups: Lookups = .system) {
        self.lookups = lookups
    }

    /// Build the menu entries for `url`. The user's default app
    /// (if any) appears first; remaining apps are alphabetised by
    /// display name. Duplicates by `appURL.standardizedFileURL.path`
    /// are removed so a single bundle with multiple symlinks doesn't
    /// double-up in the menu.
    func entries(for url: URL) -> [OpenWithEntry] {
        let allApps = lookups.allAppsForURL(url)
        let defaultApp = lookups.defaultAppForURL(url)

        var seen = Set<String>()
        var defaultEntry: OpenWithEntry?
        var others: [OpenWithEntry] = []

        for appURL in allApps {
            let key = appURL.standardizedFileURL.path
            guard seen.insert(key).inserted else { continue }
            let isDefault = defaultApp.map {
                $0.standardizedFileURL.path == key
            } ?? false
            let entry = OpenWithEntry(
                appURL: appURL,
                displayName: lookups.displayName(appURL),
                icon: lookups.icon(appURL),
                isDefault: isDefault
            )
            if isDefault {
                defaultEntry = entry
            } else {
                others.append(entry)
            }
        }

        // The default app sometimes isn't in the `urlsForApplications`
        // result on weird configurations — synthesise its entry.
        if defaultEntry == nil, let defaultApp,
           seen.insert(defaultApp.standardizedFileURL.path).inserted {
            defaultEntry = OpenWithEntry(
                appURL: defaultApp,
                displayName: lookups.displayName(defaultApp),
                icon: lookups.icon(defaultApp),
                isDefault: true
            )
        }

        others.sort { $0.displayName.localizedCaseInsensitiveCompare($1.displayName) == .orderedAscending }
        if let defaultEntry {
            return [defaultEntry] + others
        }
        return others
    }
}
