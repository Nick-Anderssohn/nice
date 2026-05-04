//
//  Models.swift
//  Nice
//
//  Value types for the tab/pane data model. A sidebar row is a `Tab`
//  (a session); each tab owns an ordered list of `Pane`s which show up
//  as pills in the upper toolbar. Claude and terminal panes share the
//  same storage — only the `kind` field distinguishes them.
//

import Foundation

enum PaneKind: String, Hashable, Sendable, Codable {
    case claude
    case terminal
}

/// Which content the expanded sidebar is currently showing. Window-global
/// (one mode at a time per window) and bridged to `@SceneStorage` upstream
/// in `AppShellView` so each window restores its mode across relaunch.
enum SidebarMode: String, Hashable, Sendable, Codable {
    /// Default — projects and tabs.
    case tabs
    /// File system browser rooted at the active tab's CWD.
    case files
}

enum TabStatus: String, CaseIterable, Hashable, Sendable, Codable {
    case thinking
    case waiting
    case idle
}

struct Pane: Identifiable, Hashable, Sendable, Codable {
    let id: String
    var title: String
    var kind: PaneKind
    /// False once the pty for this pane has exited. `AppState` flips this
    /// before removing the pane so interim UI states render cleanly.
    var isAlive: Bool = true
    /// Per-pane status (thinking/waiting/idle). Meaningful for `.claude`
    /// panes; `.terminal` panes stay `.idle`.
    var status: TabStatus = .idle
    /// For `.waiting`: whether the user has already seen this pane enter
    /// the waiting state. The sidebar/toolbar dot pulses for waiting only
    /// while this is `false` — once the user looks at the pane, we stop
    /// competing for their attention. Recomputed on every entry into
    /// `.waiting` (so a fresh waiting event can pulse again) and cleared
    /// on any transition out of `.waiting`.
    var waitingAcknowledged: Bool = false
    /// Runtime-only: true while this pane is hosting a live `claude`
    /// process (as opposed to a shell that happens to be rendered as a
    /// Claude pane — e.g. a restored pane pre-typed with `claude --resume`
    /// that the user hasn't hit Enter on yet). Drives the socket-
    /// promotion logic: a sidebar tab with no running Claude reroutes
    /// the next in-tab `claude` into the current pane instead of opening
    /// a new tab. Excluded from `Codable` — restored tabs always come
    /// back `false`, which is what we want.
    var isClaudeRunning: Bool = false
    /// Last-observed cwd for this pane's shell, captured from OSC 7
    /// emitted by the injected `chpwd_functions` hook. Persisted so a
    /// relaunched pane respawns where the user left it. `nil` until the
    /// shell has emitted at least one OSC 7 — callers fall back to
    /// `Tab.cwd` in that case.
    var cwd: String? = nil

    private enum CodingKeys: String, CodingKey {
        case id, title, kind, isAlive, status, waitingAcknowledged, cwd
    }
}

extension Pane {
    /// Apply a status transition and recompute `waitingAcknowledged` as a
    /// side-effect: entering `.waiting` marks it acknowledged iff the
    /// user is already looking at this pane; any other status resets it.
    /// No-op when `newStatus` matches the current status so re-reporting
    /// the same state doesn't clobber a prior acknowledgment.
    mutating func applyStatusTransition(
        to newStatus: TabStatus,
        isCurrentlyBeingViewed: Bool
    ) {
        guard status != newStatus else { return }
        status = newStatus
        switch newStatus {
        case .waiting:
            waitingAcknowledged = isCurrentlyBeingViewed
        case .thinking, .idle:
            waitingAcknowledged = false
        }
    }

    /// The user just looked at this pane — if it is waiting, dismiss the
    /// attention signal. No-op otherwise.
    mutating func markAcknowledgedIfWaiting() {
        if status == .waiting {
            waitingAcknowledged = true
        }
    }

    /// Whether this pane is currently competing for the user's attention.
    /// Mirrors the rule the sidebar/toolbar status dots use: `.thinking`
    /// always counts; `.waiting` counts only until the user has looked
    /// at it (`waitingAcknowledged`); `.idle` never counts.
    var needsAttention: Bool {
        switch status {
        case .thinking: return true
        case .waiting:  return !waitingAcknowledged
        case .idle:     return false
        }
    }
}

struct Tab: Identifiable, Hashable, Sendable, Codable {
    let id: String
    var title: String
    var cwd: String
    var branch: String?
    /// Ordered panes shown as pills in the toolbar. Expected non-empty
    /// while the tab is alive; the invariant is maintained by `AppState`.
    var panes: [Pane] = []
    /// The pane currently focused in the toolbar. `nil` only when
    /// `panes` is empty (transient, during teardown).
    var activePaneId: String? = nil
    /// True once `paneTitleChanged` has filled the tab label from Claude's
    /// OSC-set title rather than leaving it as "New tab".
    var titleAutoGenerated: Bool = false
    /// True once the user has explicitly renamed this tab via the sidebar
    /// inline editor. When true, `applyAutoTitle` skips this tab so
    /// Claude's OSC-driven titles can't clobber the user's choice.
    var titleManuallySet: Bool = false
    /// UUID of the underlying Claude Code session, so the tab can be
    /// resumed across Nice relaunches via `claude --resume <uuid>`.
    /// Set at tab creation (we pass `--session-id <uuid>` to claude so
    /// the CLI writes its transcript under the expected UUID), `nil`
    /// for terminal-only tabs (including everything in the Terminals
    /// group).
    var claudeSessionId: String? = nil
    /// Monotonically incremented per-tab counter feeding the auto-name
    /// "Terminal N" when a new terminal pane is added. Never decremented
    /// when a pane is closed, so a closed "Terminal 2" does not get
    /// reused — the next add becomes "Terminal 4". Persisted via
    /// PersistedTab so the counter survives relaunch.
    var nextTerminalIndex: Int = 1
}

extension Tab {
    /// Recover `nextTerminalIndex` from a tab's pane titles when an
    /// older session file lacks the persisted counter. Parses each
    /// title against `^terminal\s+(\d+)$` (case-insensitive) and
    /// returns `1 + max(N)`, floored at 1 so a tab whose terminal
    /// panes have all been renamed still starts numbering from 1.
    /// Pure function — exposed so the hydration path stays a single
    /// model-aware call site instead of duplicating regex grammar.
    static func recoverNextTerminalIndex(fromPaneTitles titles: [String]) -> Int {
        let regex = try? NSRegularExpression(
            pattern: #"^terminal\s+(\d+)$"#,
            options: .caseInsensitive
        )
        let maxN = titles.compactMap { title -> Int? in
            let range = NSRange(title.startIndex..., in: title)
            guard let match = regex?.firstMatch(in: title, range: range),
                  let capture = Range(match.range(at: 1), in: title)
            else { return nil }
            return Int(title[capture])
        }.max() ?? 0
        return max(1, maxN + 1)
    }

    /// True if any alive pane on this tab is a Claude pane.
    var hasClaude: Bool {
        panes.contains { $0.kind == .claude && $0.isAlive }
    }

    /// The pane currently focused, if any.
    var activePane: Pane? {
        guard let id = activePaneId else { return nil }
        return panes.first { $0.id == id }
    }

    /// Whether any pane in `offscreenIds` currently needs attention. Used
    /// by the toolbar's overflow chevron to badge itself when an attention-
    /// worthy pane has scrolled out of view. Visible panes are excluded
    /// because their pill's own `StatusDot` already pulses for the user.
    func hasOffscreenAttention(offscreenIds: Set<String>) -> Bool {
        guard !offscreenIds.isEmpty else { return false }
        return panes.contains { pane in
            offscreenIds.contains(pane.id) && pane.needsAttention
        }
    }

    /// Aggregate status shown in the sidebar dot. Derived from live
    /// Claude panes so the sidebar can't drift from the toolbar pill
    /// (which reads `Pane.status` directly). The app maintains "at most
    /// one Claude pane per tab", so in practice this reduces to that
    /// pane's status — the aggregation is written defensively for
    /// transient multi-pane states during creation/teardown.
    var status: TabStatus {
        let live = panes.filter { $0.kind == .claude && $0.isAlive }
        if live.contains(where: { $0.status == .thinking }) { return .thinking }
        if live.contains(where: { $0.status == .waiting })  { return .waiting }
        return .idle
    }

    /// Sidebar-dot pulse suppression: true iff every waiting Claude
    /// pane on the tab has been acknowledged by the user. Returns
    /// false when no Claude pane is waiting — the sidebar only reads
    /// this field while `status == .waiting`, so the value is only
    /// meaningful in that case.
    var waitingAcknowledged: Bool {
        let waiting = panes.filter {
            $0.kind == .claude && $0.isAlive && $0.status == .waiting
        }
        guard !waiting.isEmpty else { return false }
        return waiting.allSatisfy { $0.waitingAcknowledged }
    }
}

struct Project: Identifiable, Hashable, Sendable, Codable {
    let id: String
    var name: String
    var path: String
    var tabs: [Tab]
}

extension Project {
    static var seed: [Project] { [] }
}
