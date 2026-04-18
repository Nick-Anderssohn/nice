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

struct Tab: Identifiable, Hashable, Sendable {
    let id: String
    var title: String
    var status: TabStatus
    var cwd: String
    var branch: String?
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
        [
            Project(
                id: "novel",
                name: "novel-web",
                path: "~/code/novel-web",
                tabs: [
                    Tab(id: "t1", title: "Auth refactor — JWT rotation",
                        status: .thinking,
                        cwd: "~/code/novel-web",
                        branch: "feat/auth-rotation"),
                    Tab(id: "t2", title: "Flaky E2E cleanup",
                        status: .waiting,
                        cwd: "~/code/novel-web",
                        branch: "main"),
                    Tab(id: "t3", title: "Migrate to pnpm workspaces",
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
                    Tab(id: "t4", title: "Nightly reconciliation bug",
                        status: .thinking,
                        cwd: "~/code/ledger-api",
                        branch: "fix/recon"),
                    Tab(id: "t5", title: "Postgres → Neon migration notes",
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
                    Tab(id: "t6", title: "Explain eBPF tracing",
                        status: .idle,
                        cwd: "~/scratch",
                        branch: nil),
                    Tab(id: "t7", title: "Regex for ISO-8601 durations",
                        status: .idle,
                        cwd: "~/scratch",
                        branch: nil),
                ]
            ),
        ]
    }
}
