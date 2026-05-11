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
        let initialPane = Pane(id: initialPaneId, title: "Terminal 1", kind: .terminal)
        var mainTab = Tab(
            id: mainTabId,
            title: "Main",
            cwd: initialMainCwd,
            branch: nil,
            panes: [initialPane],
            activePaneId: initialPaneId
        )
        mainTab.nextTerminalIndex = 2
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
    ///
    /// Skips the write-back when the closure leaves the tab byte-equal
    /// to its prior state. With `@Observable`, assigning to
    /// `projects[pi].tabs[ti]` always fires an observation notification
    /// on `projects`, even when the new value is identical — and any
    /// view that reads `projects` (the file browser header reads
    /// `fileBrowserHeaderTitle`, which calls `tab(for:)`, which walks
    /// `projects`) will then re-evaluate. SwiftUI re-evaluating the
    /// parent of an open `.contextMenu` replaces the bridged
    /// `NSMenuItem` for any nested `Menu` view, which dismisses the
    /// currently-shown submenu. Repeated callers (Claude's title
    /// spinner, OSC 7 chpwd echoes) would otherwise dismiss/redraw the
    /// "Open With" submenu about once a second while a Claude pane is
    /// thinking.
    @discardableResult
    func mutateTab(id: String, _ transform: (inout Tab) -> Void) -> Bool {
        guard let (pi, ti) = projectTabIndex(for: id) else { return false }
        var copy = projects[pi].tabs[ti]
        transform(&copy)
        if copy != projects[pi].tabs[ti] {
            projects[pi].tabs[ti] = copy
        }
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

    /// Remove the tab at `(projectIndex, tabIndex)` from the model and
    /// sweep any sibling `parentTabId` references that pointed at it.
    /// Returns the removed tab so callers can use it for cleanup
    /// (pty teardown, file-browser state, project-empty checks).
    ///
    /// Single removal entry point: every tab-removal path must funnel
    /// through here so the parent-pointer sweep can't be skipped.
    /// Inlining `tabs.projects[pi].tabs.remove(at:)` at a new call
    /// site would orphan /branch children with a dangling
    /// `parentTabId` — they'd still render indented under a tab that
    /// doesn't exist, and the sidebar's `tab(for:)` lookup would
    /// silently return nil for the parent. The dissolve cascade in
    /// `AppState.finalizeDissolvedTab` is the only production caller
    /// today; future close paths must reach for this method too.
    @discardableResult
    func removeTab(projectIndex pi: Int, tabIndex ti: Int) -> Tab {
        let removed = projects[pi].tabs.remove(at: ti)
        clearDanglingParentReferences(to: removed.id)
        return removed
    }

    /// Clear `parentTabId` on every tab that pointed at `removedTabId`.
    /// Internal helper for `removeTab` and the legacy direct callers
    /// in tests; production code should reach for `removeTab` instead
    /// so the array remove and the sweep stay atomic. Walks every
    /// project so a (rare) cross-project move that left a stale link
    /// still gets cleaned up.
    func clearDanglingParentReferences(to removedTabId: String) {
        for pi in projects.indices {
            for ti in projects[pi].tabs.indices {
                if projects[pi].tabs[ti].parentTabId == removedTabId {
                    projects[pi].tabs[ti].parentTabId = nil
                }
            }
        }
    }

    /// Insert a fresh "branch parent" tab into the same project as
    /// `originatingTabId`, applying the depth-1 lineage rule. Mirrors
    /// the shape `createTabFromMainTerminal` produces (claude pane +
    /// companion terminal) but the claude pane is NOT marked running:
    /// the deferred-resume path spawns a plain shell with `claude
    /// --resume <oldSessionId>` pre-typed via `print -z`, so nothing
    /// actually runs (and no tokens are spent) until the user opens
    /// the new tab and presses Enter.
    ///
    /// Lineage layout — depth-1 tree under the original:
    ///   • If the originating tab already has a `parentTabId`, the
    ///     new parent inherits it (becomes a sibling under the same
    ///     root).
    ///   • Otherwise the new parent BECOMES the root. The originating
    ///     tab and every tab that was already pointing at it (its
    ///     former depth-1 children) are re-parented to the new root
    ///     so the depth-1 invariant survives subsequent /branches.
    ///
    /// The new parent inherits the originating tab's title-state
    /// fields so the live OSC stream from the originating tab won't
    /// retroactively rename the parent — the parent is a frozen
    /// snapshot of the pre-/branch session, not a mirror of where
    /// the active conversation drifts to next.
    ///
    /// Returns the inserted tab so callers can spawn its pty against
    /// the resolved `cwd`. Returns nil when the originating tab can't
    /// be found or lives in the pinned Terminals project (which
    /// never hosts Claude sessions; a /branch firing from there
    /// would already be a model violation, but we guard defensively).
    ///
    /// Same-project precondition: `Tab.parentTabId` is constrained
    /// to reference a tab inside the same project. The renderer
    /// indents children under their parent within the same project
    /// tree; a cross-project pointer would render an indent under
    /// nothing. The originating tab's lineage root (when present) is
    /// asserted to live in the same project; the inserted parent
    /// inherits that root or becomes the new root, both within the
    /// originating tab's project.
    func insertBranchParent(
        forTabId originatingTabId: String,
        newTabId: String,
        claudePaneId: String,
        terminalPaneId: String,
        oldSessionId: String
    ) -> Tab? {
        guard let (pi, ti) = projectTabIndex(for: originatingTabId),
              !isTerminalsProjectTab(originatingTabId)
        else { return nil }
        let originating = projects[pi].tabs[ti]
        let inheritedRoot = originating.parentTabId
        if let root = inheritedRoot {
            // Defensive: parentTabId is a within-project reference. A
            // cross-project pointer would mean a prior bug
            // (cross-project moveTab, hand-edited sessions.json) has
            // already corrupted state; don't compound it by inserting
            // a sibling that quietly inherits the bad pointer.
            assert(
                projects[pi].tabs.contains(where: { $0.id == root }),
                "originating tab's parentTabId '\(root)' must live in the same project"
            )
        }

        var claudePane = Pane(id: claudePaneId, title: "Claude", kind: .claude)
        claudePane.isClaudeRunning = false
        var parentTab = Tab(
            id: newTabId,
            title: originating.title,
            cwd: originating.cwd,
            branch: originating.branch,
            panes: [
                claudePane,
                Pane(id: terminalPaneId, title: "Terminal 1", kind: .terminal),
            ],
            activePaneId: claudePaneId,
            titleAutoGenerated: originating.titleAutoGenerated,
            titleManuallySet: originating.titleManuallySet,
            claudeSessionId: oldSessionId,
            parentTabId: inheritedRoot
        )
        // Seed has "Terminal 1"; the next add should be "Terminal 2"
        // — same convention as createTabFromMainTerminal /
        // createClaudeTabInProject.
        parentTab.nextTerminalIndex = 2

        // Insert immediately above the originating tab so the visual
        // order reads [parent, child].
        projects[pi].tabs.insert(parentTab, at: ti)

        if inheritedRoot == nil {
            // First-branch root promotion: the new parent becomes
            // the root. Re-parent the originating tab and every tab
            // that was already pointing at it to the new root so the
            // depth-1 invariant survives. Without this the former
            // root would slide to depth-1 while its former children
            // still pointed at it, putting them effectively at
            // depth-2 in the lineage.
            for j in projects[pi].tabs.indices {
                let here = projects[pi].tabs[j]
                if here.id == originatingTabId
                    || here.parentTabId == originatingTabId {
                    projects[pi].tabs[j].parentTabId = newTabId
                }
            }
        }

        return parentTab
    }

    /// Sweep every `parentTabId` against the set of currently-present
    /// tab ids and clear any that point at a tab that doesn't exist.
    /// Called from `WindowSession.restoreSavedWindow` after the full
    /// tree has been rebuilt from the snapshot, so a hand-edited or
    /// partially-corrupt sessions.json (parent removed by hand, or a
    /// prior crash mid-/branch persisted the child but not the parent)
    /// can't leave a child rendering one indent deep under a tab that
    /// doesn't exist. Safe to call multiple times — pure cleanup, no
    /// side effects beyond the field clears.
    func pruneDanglingParentReferences() {
        var validIds = Set<String>()
        for project in projects {
            for tab in project.tabs {
                validIds.insert(tab.id)
            }
        }
        for pi in projects.indices {
            for ti in projects[pi].tabs.indices {
                if let parent = projects[pi].tabs[ti].parentTabId,
                   !validIds.contains(parent) {
                    projects[pi].tabs[ti].parentTabId = nil
                }
            }
        }
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

    /// Default display title for a pane of `kind`. Terminal panes use
    /// the tab's monotonic `nextTerminalIndex` (never reused — same
    /// policy `addPane` enforces). Used by `renamePane`'s empty-submit
    /// reset path; constructor sites today still hand-write the same
    /// strings, but this is the single source of truth.
    static func defaultPaneTitle(kind: PaneKind, terminalIndex: Int) -> String {
        switch kind {
        case .claude:   return "Claude"
        case .terminal: return "Terminal \(terminalIndex)"
        }
    }

    /// User-initiated rename for an individual pane (e.g. from the
    /// inline pane-pill editor).
    ///
    /// - **Non-empty trimmed input:** sets `pane.title` and flips
    ///   `pane.titleManuallySet = true` so subsequent OSC titles
    ///   from the running program can't clobber the user's choice.
    ///   Symmetric with `renameTab` flipping `Tab.titleManuallySet`.
    /// - **Empty trimmed input:** resets the pane to its per-kind
    ///   auto-default and clears `titleManuallySet`, releasing the
    ///   lock so OSC titles drive the pill again. For terminal
    ///   panes the reset consumes and increments
    ///   `tab.nextTerminalIndex` — same monotonic-never-reuse policy
    ///   `addPane` uses (rename → reset → rename → reset cycles
    ///   climb the counter; acceptable for an unusual user gesture).
    func renamePane(tabId: String, paneId: String, to newTitle: String) {
        let trimmed = newTitle.trimmingCharacters(in: .whitespacesAndNewlines)
        var changed = false
        mutateTab(id: tabId) { tab in
            guard let idx = tab.panes.firstIndex(where: { $0.id == paneId })
            else { return }
            if trimmed.isEmpty {
                // Empty submit: release the lock and recompute the
                // auto-default. Terminal reset consumes the next slot
                // from the monotonic counter so a subsequent addPane
                // won't collide with the freshly-reset pane's name.
                let resetTitle: String
                switch tab.panes[idx].kind {
                case .claude:
                    resetTitle = TabModel.defaultPaneTitle(
                        kind: .claude, terminalIndex: 0  // unused
                    )
                case .terminal:
                    let n = tab.nextTerminalIndex
                    resetTitle = TabModel.defaultPaneTitle(
                        kind: .terminal, terminalIndex: n
                    )
                    tab.nextTerminalIndex = n + 1
                }
                if tab.panes[idx].title != resetTitle
                    || tab.panes[idx].titleManuallySet {
                    tab.panes[idx].title = resetTitle
                    tab.panes[idx].titleManuallySet = false
                    changed = true
                }
            } else {
                if tab.panes[idx].title != trimmed
                    || !tab.panes[idx].titleManuallySet {
                    tab.panes[idx].title = trimmed
                    tab.panes[idx].titleManuallySet = true
                    changed = true
                }
            }
        }
        if changed { onTreeMutation?() }
    }

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
        let pane = Pane(id: paneId, title: "Terminal 1", kind: .terminal)
        var mainTab = Tab(
            id: mainTabId,
            title: "Main",
            cwd: cwd,
            branch: nil,
            panes: [pane],
            activePaneId: paneId
        )
        mainTab.nextTerminalIndex = 2
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

    /// Update `tab.cwd` to `newCwd` and pull along any pane whose
    /// `pane.cwd` was still tracking the old `tab.cwd` (or has never
    /// been set). Preserves the cwd of a pane that has already
    /// diverged via OSC 7 — that means the user has `cd`'d the
    /// terminal companion somewhere of their own, and snapping it
    /// back into the Claude pane's new worktree would destroy that
    /// context.
    ///
    /// Returns `true` when anything actually changed (so callers can
    /// fire the right save/notify side effect), or `false` for any
    /// no-op shape: tab not found, `newCwd` equals the current
    /// `tab.cwd`. The change-detection short-circuit is what makes
    /// "every prompt sends a SessionStart-with-cwd hook" cheap —
    /// most rotations don't move the cwd and this returns false
    /// fast.
    ///
    /// Centralizes the pane-follow policy so the rotation handler
    /// (`SessionsModel.updateTabCwd`) and the restore-time heal pass
    /// (`WindowSession.addRestoredTabModel`) can't drift on what
    /// "follow the tab" means.
    @discardableResult
    func adoptTabCwd(forTabId tabId: String, newCwd: String) -> Bool {
        var changed = false
        mutateTab(id: tabId) { tab in
            let oldCwd = tab.cwd
            guard oldCwd != newCwd else { return }
            tab.cwd = newCwd
            for i in tab.panes.indices {
                let paneCwd = tab.panes[i].cwd
                if paneCwd == nil || paneCwd == oldCwd {
                    tab.panes[i].cwd = newCwd
                }
            }
            changed = true
        }
        return changed
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
