//
//  Models.swift
//  Nice
//
//  Value types mirroring the React mock in
//  /tmp/nice-design/nice/project/nice/data.jsx. Phase 2 — sidebar only.
//  Keep field names aligned with the JSX so the seed data stays legible.
//

import Foundation

enum TabStatus: String, CaseIterable, Hashable, Sendable {
    case thinking
    case waiting
    case idle
}

struct CompanionTerminal: Identifiable, Hashable, Sendable {
    let id: String
    var title: String
}

struct Tab: Identifiable, Hashable, Sendable {
    let id: String
    var title: String
    var status: TabStatus
    var cwd: String
    var branch: String?
    /// Flips false after the Claude process in this tab exits. The tab
    /// stays in the sidebar; the UI swaps its icon and hides the chat
    /// pane. Defaults to true because new tabs spawn with Claude alive.
    var hasClaudePane: Bool = true
    /// One or more companion zsh terminals hosted next to the Claude
    /// pane. At least one entry is expected while the tab is alive;
    /// the invariant is maintained by `AppState`.
    var companions: [CompanionTerminal] = []
    /// The companion currently focused in the tab bar. `nil` only when
    /// `companions` is empty (transient, during teardown).
    var activeCompanionId: String? = nil
}

struct Project: Identifiable, Hashable, Sendable {
    let id: String
    var name: String
    var path: String
    var tabs: [Tab]
}

extension Project {
    static var seed: [Project] { [] }
}
