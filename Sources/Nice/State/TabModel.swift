//
//  TabModel.swift
//  Nice
//
//  The per-window data model: the projects/tabs/panes tree plus which
//  tab is currently selected. Carved out of `AppState` so the document
//  shape can be reasoned about (and unit-tested) without dragging in
//  pty plumbing, the control socket, theme caches, or persistence.
//
//  Pure value-tree: nothing here spawns a process, opens a socket, or
//  writes to disk. Side-effectful concerns (saving on mutation, spawning
//  a session for a freshly-seeded Terminals tab, ack'ing waiting state
//  on the active pane's pty) are routed back to `AppState` via the
//  `onTreeMutation` callback or by returning a `SeedResult` describing
//  what the caller still needs to do.
//
//  The "Terminals" group at the top of the sidebar is a regular
//  `Project` with the reserved id `TabModel.terminalsProjectId`. It is
//  always present at index 0 and cannot be removed by the user, but
//  its tabs are ordinary `Tab` values with terminal-only panes.
//

import AppKit
import Foundation
import Observation

@MainActor
@Observable
final class TabModel {
    /// Reserved id for the pinned Terminals project at index 0 of
    /// `projects`. The project is always present and cannot be deleted
    /// by the user; its tabs are ordinary terminal-only tabs.
    static let terminalsProjectId = "terminals"
    /// Stable id for the default "Main" tab seeded into the Terminals
    /// project on fresh launches. UI tests key off a `sidebar.terminals`
    /// accessibility alias on this tab.
    static let mainTerminalTabId = "terminals-main"

    var projects: [Project]

    /// Currently-selected tab. Defaults to the Main terminal tab on
    /// launch.
    var activeTabId: String? {
        didSet {
            // Viewing a tab dismisses the attention pulse on its active
            // pane's waiting state — centralised here so every call site
            // that flips `activeTabId` gets the same acknowledgment.
            if let id = activeTabId, id != oldValue {
                acknowledgeWaitingOnActivePane(tabId: id)
                onTreeMutation?()
            }
        }
    }

    /// Fired whenever the tree changes in a way the owning AppState
    /// should react to (today: schedule a debounced session save).
    /// AppState wires this in `init`. Held weakly via the closure's
    /// own capture pattern; the model itself takes no `unowned`/`weak`
    /// references back.
    @ObservationIgnored
    var onTreeMutation: (() -> Void)?

    init(initialMainCwd: String) {
        // Seed the pinned Terminals project with one "Main" tab hosting
        // a single terminal pane. Pty spawn is deferred to the owning
        // AppState so this initializer stays side-effect free.
        let mainTabId = Self.mainTerminalTabId
        let initialPaneId = "\(mainTabId)-p\(Int(Date().timeIntervalSince1970 * 1000))"
        let initialPane = Pane(id: initialPaneId, title: "zsh", kind: .terminal)
        let mainTab = Tab(
            id: mainTabId,
            title: "Main",
            cwd: initialMainCwd,
            branch: nil,
            panes: [initialPane],
            activePaneId: initialPaneId
        )
        let terminalsProject = Project(
            id: Self.terminalsProjectId,
            name: "Terminals",
            path: initialMainCwd,
            tabs: [mainTab]
        )
        self.projects = [terminalsProject]
        self.activeTabId = mainTabId
    }

    // MARK: - Lookup

    /// Look up a tab by id across every project, including the pinned
    /// Terminals group.
    func tab(for id: String) -> Tab? {
        for project in projects {
            if let hit = project.tabs.first(where: { $0.id == id }) {
                return hit
            }
        }
        return nil
    }

    /// Mutate the tab identified by `id` in place. Calls `transform`
    /// with the right backing storage (Terminals tab, or an element of
    /// `projects`). Returns true if the tab was found.
    @discardableResult
    func mutateTab(id: String, _ transform: (inout Tab) -> Void) -> Bool {
        guard let (pi, ti) = projectTabIndex(for: id) else { return false }
        transform(&projects[pi].tabs[ti])
        return true
    }

    /// Project + tab index for the tab with id `id`, for in-place
    /// mutation in the `projects` array.
    func projectTabIndex(for id: String) -> (Int, Int)? {
        for (pi, project) in projects.enumerated() {
            if let ti = project.tabs.firstIndex(where: { $0.id == id }) {
                return (pi, ti)
            }
        }
        return nil
    }

    /// First tab id in sidebar order (Terminals project, then project
    /// tabs). Used to fall back to a sensible selection when the
    /// active tab dissolves. Returns nil when no tab exists anywhere.
    func firstAvailableTabId() -> String? {
        for project in projects {
            if let id = project.tabs.first?.id { return id }
        }
        return nil
    }

    /// Project that owns the given tab, or `nil` if no such tab is
    /// currently in the model.
    private func project(forTab id: String) -> Project? {
        for project in projects where project.tabs.contains(where: { $0.id == id }) {
            return project
        }
        return nil
    }

    /// Title to show at the top of the file browser for `tabId`.
    /// Encapsulates the rule "use the owning project's name unless
    /// the tab is in the pinned Terminals project (whose name is
    /// generic), in which case fall back to the tab's own title."
    func fileBrowserHeaderTitle(forTab id: String) -> String {
        let tabTitle = tab(for: id)?.title
        guard let project = project(forTab: id) else {
            return tabTitle ?? "Files"
        }
        if project.id == Self.terminalsProjectId {
            return tabTitle ?? project.name
        }
        return project.name
    }

    /// True when `tabId` lives inside the pinned Terminals project.
    func isTerminalsProjectTab(_ tabId: String) -> Bool {
        guard let terminals = projects.first(where: { $0.id == Self.terminalsProjectId }) else {
            return false
        }
        return terminals.tabs.contains { $0.id == tabId }
    }

    /// Snapshot of this window's live panes grouped by kind. Used by
    /// the quit / window-close confirmation alerts to word the prompt
    /// without exposing the model to callers outside AppState.
    var livePaneCounts: (claude: Int, terminal: Int) {
        var claude = 0
        var terminal = 0
        for project in projects {
            for tab in project.tabs {
                for pane in tab.panes where pane.isAlive {
                    switch pane.kind {
                    case .claude: claude += 1
                    case .terminal: terminal += 1
                    }
                }
            }
        }
        return (claude, terminal)
    }

    /// Flat list of sidebar tab ids in displayed order. The pinned
    /// Terminals project is always first, so its tabs lead; project
    /// tabs follow in project/then-tab order. Used by the keyboard
    /// shortcut handlers to walk a deterministic visible set.
    var navigableSidebarTabIds: [String] {
        projects.flatMap { $0.tabs.map(\.id) }
    }

    /// Find the tab whose pane list contains `paneId`.
    func tabIdOwning(paneId: String) -> String? {
        for project in projects {
            for tab in project.tabs {
                if tab.panes.contains(where: { $0.id == paneId }) {
                    return tab.id
                }
            }
        }
        return nil
    }

    // MARK: - Selection

    func selectTab(_ id: String) {
        activeTabId = id
    }

    /// Move focus to the next sidebar tab, wrapping. No-op when there's
    /// only one navigable tab (Terminals alone).
    func selectNextSidebarTab() { stepSidebarTab(by: +1) }

    /// Move focus to the previous sidebar tab, wrapping.
    func selectPrevSidebarTab() { stepSidebarTab(by: -1) }

    private func stepSidebarTab(by offset: Int) {
        let ids = navigableSidebarTabIds
        guard ids.count > 1 else { return }
        let currentIdx = activeTabId.flatMap { ids.firstIndex(of: $0) } ?? 0
        let nextIdx = ((currentIdx + offset) % ids.count + ids.count) % ids.count
        activeTabId = ids[nextIdx]
    }

    /// Clear the waiting-attention pulse on whichever pane is currently
    /// focused in `tabId`. Called from the `activeTabId` `didSet` when
    /// the user navigates to a different tab.
    private func acknowledgeWaitingOnActivePane(tabId: String) {
        mutateTab(id: tabId) { tab in
            guard let paneId = tab.activePaneId,
                  let pi = tab.panes.firstIndex(where: { $0.id == paneId })
            else { return }
            tab.panes[pi].markAcknowledgedIfWaiting()
        }
    }

    // MARK: - Reordering

    /// Move `tabId` to a new slot within the same project, relative to
    /// `targetTabId`: either just before it (`placeAfter == false`) or
    /// just after it. No-op when the two tabs aren't in the same
    /// project, when either id is unknown, or when the move wouldn't
    /// change order. Tabs inside the pinned Terminals project reorder
    /// like any other project's tabs.
    func moveTab(_ tabId: String, relativeTo targetTabId: String, placeAfter: Bool) {
        guard tabId != targetTabId else { return }
        guard let (srcProject, srcIndex) = projectTabIndex(for: tabId),
              let (dstProject, dstIndex) = projectTabIndex(for: targetTabId),
              srcProject == dstProject
        else { return }
        // `placeAfter` picks the slot just past the target; then account
        // for the fact that removing the source first shifts everything
        // after it down by one.
        var insertIndex = placeAfter ? dstIndex + 1 : dstIndex
        if srcIndex < insertIndex { insertIndex -= 1 }
        guard insertIndex != srcIndex else { return }
        let tab = projects[srcProject].tabs.remove(at: srcIndex)
        projects[srcProject].tabs.insert(tab, at: insertIndex)
        onTreeMutation?()
    }

    /// Mirrors `moveTab` without mutating — returns true iff the drop
    /// would actually reorder. The sidebar drop indicator uses this to
    /// suppress the insertion line for no-op drops.
    func wouldMoveTab(_ tabId: String, relativeTo targetTabId: String, placeAfter: Bool) -> Bool {
        guard tabId != targetTabId,
              let (srcProject, srcIndex) = projectTabIndex(for: tabId),
              let (dstProject, dstIndex) = projectTabIndex(for: targetTabId),
              srcProject == dstProject
        else { return false }
        var insertIndex = placeAfter ? dstIndex + 1 : dstIndex
        if srcIndex < insertIndex { insertIndex -= 1 }
        return insertIndex != srcIndex
    }

    // MARK: - Title application

    /// User-initiated rename from the sidebar inline editor. Trims
    /// whitespace, ignores empty input, and marks the tab so subsequent
    /// `applyAutoTitle` calls skip it.
    func renameTab(id tabId: String, to newTitle: String) {
        let trimmed = newTitle.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        var changed = false
        mutateTab(id: tabId) { tab in
            if tab.title != trimmed || !tab.titleManuallySet {
                tab.title = trimmed
                tab.titleManuallySet = true
                changed = true
            }
        }
        if changed { onTreeMutation?() }
    }

    /// Apply a Claude-generated session title to the tab. Humanizes the
    /// kebab-case string Claude records (e.g. "fix-top-bar-height") into
    /// sentence-case ("Fix top bar height"). Skipped entirely once the
    /// user has manually renamed the tab, so late-arriving auto-titles
    /// can't clobber a user edit. The guard is keyed on `tabId`, so
    /// manually renaming one tab never affects another tab's flow.
    func applyAutoTitle(tabId: String, rawTitle: String) {
        guard let existing = tab(for: tabId), !existing.titleManuallySet else {
            return
        }
        let humanized = Self.humanizeSessionTitle(rawTitle)
        guard !humanized.isEmpty else { return }
        var changed = false
        mutateTab(id: tabId) { tab in
            if tab.title != humanized {
                tab.title = humanized
                changed = true
            }
            tab.titleAutoGenerated = true
        }
        if changed { onTreeMutation?() }
    }

    private static func humanizeSessionTitle(_ raw: String) -> String {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return "" }
        let pieces = trimmed
            .split(whereSeparator: { $0 == "-" || $0 == "_" })
            .map(String.init)
        guard !pieces.isEmpty else { return "" }
        var joined = pieces.joined(separator: " ")
        if let first = joined.first, first.isLowercase {
            joined = first.uppercased() + joined.dropFirst()
        }
        if joined.count > 40 {
            let idx = joined.index(joined.startIndex, offsetBy: 40)
            joined = String(joined[..<idx]).trimmingCharacters(in: .whitespaces)
        }
        return joined
    }

    // MARK: - Project structure

    /// Guarantee a pinned Terminals project sits at `projects[0]`. If
    /// it's absent (first launch of this build, or a restore adopted
    /// a snapshot predating the Terminals group), synthesize one with
    /// a "Main" tab holding a fresh terminal pane. If it's present
    /// but not at index 0, move it.
    ///
    /// `spawnHook` is invoked exactly once with the synthesized
    /// `Tab` when the project had to be created from scratch — the
    /// caller (`WindowSession.ensureTerminalsProjectSeededAndSpawn`)
    /// uses this to spawn a pty for the freshly-minted Main tab.
    /// The hook is *not* called when an existing Terminals project
    /// was just reordered. This model intentionally has no pty
    /// knowledge; the hook is the one-way bridge into pty-aware
    /// callers.
    func ensureTerminalsProjectSeeded(spawnHook: (Tab) -> Void = { _ in }) {
        if let idx = projects.firstIndex(where: { $0.id == Self.terminalsProjectId }) {
            if idx != 0 {
                let project = projects.remove(at: idx)
                projects.insert(project, at: 0)
            }
            if activeTabId == nil, let firstId = projects[0].tabs.first?.id {
                activeTabId = firstId
            }
            return
        }

        let cwd = NSHomeDirectory()
        let mainTabId = Self.mainTerminalTabId
        let paneId = "\(mainTabId)-p\(Int(Date().timeIntervalSince1970 * 1000))"
        let pane = Pane(id: paneId, title: "zsh", kind: .terminal)
        let mainTab = Tab(
            id: mainTabId,
            title: "Main",
            cwd: cwd,
            branch: nil,
            panes: [pane],
            activePaneId: paneId
        )
        let project = Project(
            id: Self.terminalsProjectId,
            name: "Terminals",
            path: cwd,
            tabs: [mainTab]
        )
        projects.insert(project, at: 0)
        if activeTabId == nil {
            activeTabId = mainTabId
        }
        spawnHook(mainTab)
    }

    /// Look up `projects` by saved id; append a fresh `Project` with
    /// the saved name/path if absent. Returns the index of the
    /// matched-or-appended project.
    func ensureProject(id: String, name: String, path: String) -> Int {
        if let existing = projects.firstIndex(where: { $0.id == id }) {
            return existing
        }
        projects.append(Project(id: id, name: name, path: path, tabs: []))
        return projects.count - 1
    }

    /// Bucket `tab` into the project that anchors `cwd`'s git repo,
    /// creating a new project at the git root when none matches. Falls
    /// back to longest-prefix matching when `cwd` is not inside any
    /// git repo, preserving the legacy behavior for ad-hoc non-repo
    /// directories.
    func addTabToProjects(_ tab: Tab, cwd: String) {
        let normalizedCwd = Self.expandTilde(cwd)
        if let gitRoot = Self.findGitRoot(forCwd: normalizedCwd) {
            appendOrInsert(tab, intoProjectAt: gitRoot)
            return
        }
        // No git root: legacy longest-prefix behavior. Excludes the
        // pinned Terminals group, whose path is seeded from the Main
        // Terminal's cwd (typically $HOME) and would otherwise prefix-
        // match almost any cwd and swallow new Claude tabs that belong
        // in a fresh project group.
        if let idx = projects.enumerated()
            .filter({ $0.element.id != Self.terminalsProjectId })
            .filter({ normalizedCwd.hasPrefix(Self.expandTilde($0.element.path)) })
            .max(by: { $0.element.path.count < $1.element.path.count })?
            .offset
        {
            projects[idx].tabs.append(tab)
        } else {
            appendNewProject(at: normalizedCwd, with: tab)
        }
    }

    /// Append `tab` to the existing non-Terminals project rooted at
    /// `path`, or create a new project there if none matches.
    func appendOrInsert(_ tab: Tab, intoProjectAt path: String) {
        if let idx = firstIndex(ofNonTerminalsProjectAt: path) {
            projects[idx].tabs.append(tab)
        } else {
            appendNewProject(at: path, with: tab)
        }
    }

    /// Index of the first non-Terminals project whose `path` (after
    /// `expandTilde`) equals `path`. Single source of truth for
    /// project lookup by anchor.
    private func firstIndex(ofNonTerminalsProjectAt path: String) -> Int? {
        projects.firstIndex {
            $0.id != Self.terminalsProjectId
                && Self.expandTilde($0.path) == path
        }
    }

    /// Append a fresh project rooted at `path`, deriving the display
    /// name from the path's last component. Uses a UUID prefix instead
    /// of a timestamp so back-to-back appends in the same millisecond
    /// (e.g. inside the repair tab-move loop) can't collide on `id`.
    private func appendNewProject(at path: String, with tab: Tab) {
        let dirName = (path as NSString).lastPathComponent.uppercased()
        let projectId = "p-\(dirName.lowercased())-\(UUID().uuidString.prefix(8).lowercased())"
        projects.append(Project(
            id: projectId, name: dirName, path: path, tabs: [tab]
        ))
    }

    /// Self-heal the persisted project structure. Idempotent. Skips
    /// the pinned Terminals project entirely.
    ///
    /// Four passes:
    /// 1. Promote each non-Terminals project's `path` to its enclosing
    ///    git root if `path` is a strict descendant of one.
    /// 2. Move tabs whose own git-root anchor (computed from
    ///    `tab.cwd`) differs from the containing project's path. Tabs
    ///    whose `cwd` no longer exists on disk stay put.
    /// 3. Merge non-Terminals projects that ended up at the same
    ///    expanded path (lowest-index wins; later dupes are emptied).
    /// 4. Drop empty non-Terminals projects.
    func repairProjectStructure() {
        // 1. Promote project paths to their git roots.
        for i in projects.indices where projects[i].id != Self.terminalsProjectId {
            let path = Self.expandTilde(projects[i].path)
            guard FileManager.default.fileExists(atPath: path),
                  let root = Self.findGitRoot(forCwd: path),
                  root != path
            else { continue }
            projects[i].path = root
            projects[i].name = (root as NSString).lastPathComponent.uppercased()
        }

        // 2. Collect mis-bucketed tabs, then re-insert them at the
        //    right anchor. Two phases so the index-stable mutation
        //    (rewriting each project's tabs in place) finishes before
        //    we start appending new projects for unmatched anchors.
        struct Move { let tab: Tab; let targetGitRoot: String }
        var moves: [Move] = []
        for i in projects.indices where projects[i].id != Self.terminalsProjectId {
            let projectAnchor = Self.expandTilde(projects[i].path)
            var keep: [Tab] = []
            keep.reserveCapacity(projects[i].tabs.count)
            for tab in projects[i].tabs {
                let tabCwd = Self.expandTilde(tab.cwd)
                guard FileManager.default.fileExists(atPath: tabCwd) else {
                    keep.append(tab)
                    continue
                }
                let anchor = Self.findGitRoot(forCwd: tabCwd) ?? tabCwd
                if anchor == projectAnchor {
                    keep.append(tab)
                } else {
                    moves.append(Move(tab: tab, targetGitRoot: anchor))
                }
            }
            projects[i].tabs = keep
        }
        for move in moves {
            appendOrInsert(move.tab, intoProjectAt: move.targetGitRoot)
        }

        // 3. Merge duplicates targeting the same expanded path.
        var canonicalIndexByPath: [String: Int] = [:]
        var dupes: [Int] = []
        for i in projects.indices where projects[i].id != Self.terminalsProjectId {
            let key = Self.expandTilde(projects[i].path)
            if let canonical = canonicalIndexByPath[key] {
                projects[canonical].tabs.append(contentsOf: projects[i].tabs)
                dupes.append(i)
            } else {
                canonicalIndexByPath[key] = i
            }
        }
        for idx in dupes.sorted(by: >) {
            projects.remove(at: idx)
        }

        // 4. Drop empty non-Terminals projects.
        projects.removeAll {
            $0.id != Self.terminalsProjectId && $0.tabs.isEmpty
        }
    }

    // MARK: - Cwd resolution

    /// Resolve the cwd to use when spawning a pane for `tab`. Prefers
    /// `tab.cwd` (which may be a worktree path Claude Code created via
    /// `-w`), falling back to the containing project's path if the
    /// tab's cwd no longer exists on disk — covers the case where a
    /// user deleted a worktree between app launches.
    func resolvedSpawnCwd(for tab: Tab) -> String {
        let expanded = Self.expandTilde(tab.cwd)
        if FileManager.default.fileExists(atPath: expanded) { return expanded }
        if let project = projects.first(where: { p in
            p.tabs.contains(where: { $0.id == tab.id })
        }) {
            return Self.expandTilde(project.path)
        }
        return expanded
    }

    /// Resolve the cwd to use when spawning a new pane in `tab`. An
    /// explicit caller-supplied cwd wins; otherwise inherit from the
    /// currently-active pane so the new pane opens wherever the user
    /// just was. Falls back to `tab.cwd` when there is no active pane.
    func spawnCwdForNewPane(in tab: Tab, callerProvided cwd: String?) -> String {
        if let cwd { return cwd }
        if let activeId = tab.activePaneId,
           let activePane = tab.panes.first(where: { $0.id == activeId }) {
            return resolvedSpawnCwd(for: tab, pane: activePane)
        }
        return tab.cwd
    }

    /// Per-pane variant: prefers `pane.cwd` (last-observed via OSC 7)
    /// when set and still exists on disk. Falls back to the tab-level
    /// resolution when nil or pointing at a deleted directory.
    func resolvedSpawnCwd(for tab: Tab, pane: Pane) -> String {
        if let raw = pane.cwd {
            let expanded = Self.expandTilde(raw)
            if FileManager.default.fileExists(atPath: expanded) {
                return expanded
            }
        }
        return resolvedSpawnCwd(for: tab)
    }

    // MARK: - Static helpers

    static func expandTilde(_ path: String) -> String {
        if path == "~" { return NSHomeDirectory() }
        if path.hasPrefix("~/") {
            return NSHomeDirectory() + path.dropFirst(1)
        }
        return path
    }

    /// Strip any `<X>/.claude/worktrees/<name>/...` suffix and return
    /// `<X>`. A Nice-specific convention: sessions running inside a
    /// Nice-managed worktree should resolve to the parent repo, not
    /// to the worktree's own internal `.git` marker.
    static func stripNiceWorktreeSuffix(_ path: String) -> String {
        guard let range = path.range(of: "/.claude/worktrees/") else {
            return path
        }
        return String(path[..<range.lowerBound])
    }

    /// Walk up from `cwd` (after stripping any Nice worktree suffix),
    /// returning the absolute path of the nearest ancestor directory
    /// that contains a `.git` entry — matches both `.git/` (normal
    /// repo) and `.git` files (submodules and git worktrees). Returns
    /// nil if no `.git` is found before reaching the filesystem root.
    static func findGitRoot(forCwd cwd: String) -> String? {
        var current = stripNiceWorktreeSuffix(cwd)
        while !current.isEmpty && current != "/" {
            let dotGit = (current as NSString).appendingPathComponent(".git")
            if FileManager.default.fileExists(atPath: dotGit) {
                return current
            }
            let parent = (current as NSString).deletingLastPathComponent
            if parent == current { break }
            current = parent
        }
        return nil
    }

    /// Extract the value of `-w` / `--worktree` from Claude args. Only
    /// the space-delimited form is recognized (matches Claude Code's
    /// CLI). Returns nil if the flag is absent, trailing with no
    /// value, or the value is empty.
    static func extractWorktreeName(from args: [String]) -> String? {
        var i = 0
        while i < args.count {
            let a = args[i]
            if (a == "-w" || a == "--worktree") && i + 1 < args.count {
                let v = args[i + 1]
                return v.isEmpty ? nil : v
            }
            i += 1
        }
        return nil
    }

    /// Scan `args` for the session UUID the user already supplied via
    /// `--resume <id>`, `--session-id <id>`, `--resume=<id>`, or
    /// `--session-id=<id>`. Returns nil if none is present.
    static func extractClaudeSessionId(from args: [String]) -> String? {
        var i = 0
        while i < args.count {
            let a = args[i]
            if a == "--resume" || a == "--session-id" {
                if i + 1 < args.count {
                    return args[i + 1]
                }
            } else if a.hasPrefix("--resume=") {
                return String(a.dropFirst("--resume=".count))
            } else if a.hasPrefix("--session-id=") {
                return String(a.dropFirst("--session-id=".count))
            }
            i += 1
        }
        return nil
    }
}
