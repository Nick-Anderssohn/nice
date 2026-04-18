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
    /// Seed mirror of `PROJECTS` in data.jsx.
    static var seed: [Project] {
        func makeTab(
            id: String,
            title: String,
            status: TabStatus,
            cwd: String,
            branch: String?
        ) -> Tab {
            let companionId = "\(id)-c1"
            return Tab(
                id: id,
                title: title,
                status: status,
                cwd: cwd,
                branch: branch,
                hasClaudePane: true,
                companions: [CompanionTerminal(id: companionId, title: "Terminal 1")],
                activeCompanionId: companionId
            )
        }
        return [
            Project(
                id: "novel",
                name: "novel-web",
                path: "~/code/novel-web",
                tabs: [
                    makeTab(id: "t1", title: "Auth refactor — JWT rotation",
                            status: .thinking,
                            cwd: "~/code/novel-web",
                            branch: "feat/auth-rotation"),
                    makeTab(id: "t2", title: "Flaky E2E cleanup",
                            status: .waiting,
                            cwd: "~/code/novel-web",
                            branch: "main"),
                    makeTab(id: "t3", title: "Migrate to pnpm workspaces",
                            status: .idle,
                            cwd: "~/code/novel-web",
                            branch: "chore/pnpm"),
                ]
            ),
            Project(
                id: "ledger",
                name: "ledger-api",
                path: "~/code/ledger-api",
                tabs: [
                    makeTab(id: "t4", title: "Nightly reconciliation bug",
                            status: .thinking,
                            cwd: "~/code/ledger-api",
                            branch: "fix/recon"),
                    makeTab(id: "t5", title: "Postgres → Neon migration notes",
                            status: .idle,
                            cwd: "~/code/ledger-api",
                            branch: "main"),
                ]
            ),
            Project(
                id: "scratch",
                name: "scratch",
                path: "~/scratch",
                tabs: [
                    makeTab(id: "t6", title: "Explain eBPF tracing",
                            status: .idle,
                            cwd: "~/scratch",
                            branch: nil),
                    makeTab(id: "t7", title: "Regex for ISO-8601 durations",
                            status: .idle,
                            cwd: "~/scratch",
                            branch: nil),
                ]
            ),
        ]
    }
}
