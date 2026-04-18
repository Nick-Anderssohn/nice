//
//  AppState.swift
//  Nice
//
//  Phase 2 app-level state. Sidebar-only for now: projects, the selected
//  tab (or nil = main terminal), and the sidebar search query. Phase 3/4
//  will extend this with chat and terminal state.
//

import Foundation
import SwiftUI

@MainActor
final class AppState: ObservableObject {
    @Published var projects: [Project]
    /// `nil` = the "Main terminal" row is selected.
    @Published var activeTabId: String?
    @Published var sidebarQuery: String = ""

    init() {
        self.projects = Project.seed
        self.activeTabId = "t1"
    }

    func selectTab(_ id: String) {
        activeTabId = id
    }

    func selectMainTerminal() {
        activeTabId = nil
    }

    /// Prepend a freshly created tab to the first project and select it.
    /// Placeholder implementation — real tab creation (cwd, branch detection,
    /// Claude Code process spawn) lands in a later phase.
    func newTab() {
        guard !projects.isEmpty else { return }
        let newId = "t\(Int(Date().timeIntervalSince1970 * 1000))"
        let first = projects[0]
        let tab = Tab(
            id: newId,
            title: "New tab",
            status: .idle,
            cwd: first.path,
            branch: nil
        )
        projects[0].tabs.insert(tab, at: 0)
        activeTabId = newId
    }

    /// Case-insensitive title filter. Projects with zero matching tabs are
    /// dropped from the returned list (mirrors sidebar.jsx behaviour).
    var filteredProjects: [Project] {
        let q = sidebarQuery.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !q.isEmpty else { return projects }
        let needle = q.lowercased()
        return projects.compactMap { project in
            let matches = project.tabs.filter {
                $0.title.lowercased().contains(needle)
            }
            guard !matches.isEmpty else { return nil }
            var copy = project
            copy.tabs = matches
            return copy
        }
    }
}
