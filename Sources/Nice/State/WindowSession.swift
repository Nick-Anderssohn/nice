//
//  WindowSession.swift
//  Nice
//
//  Per-window identity and disk-state. Carved out of `AppState` so
//  the persistence concern (this window's `windowSessionId`, the
//  process-wide `claimedWindowIds` set, the debounced
//  `scheduleSessionSave`, the snapshot/restore round-trip) can live
//  apart from the composition root and be reasoned about on its own.
//
//  Holds weak references to the three sub-models it reads from:
//  `TabModel` (the projects/tabs/panes tree to snapshot and rebuild
//  on restore), `SessionsModel` (terminate the seed Main pty before
//  rebuilding, spawn restored Claude/terminal ptys deferred), and
//  `SidebarModel` (read `sidebarCollapsed` for the snapshot).
//  AppState owns all four so the weak refs cannot dangle in normal
//  operation.
//
//  AppState's callbacks (`tabs.onTreeMutation`,
//  `sessions.onSessionMutation`, `closer.onScheduleSave`) call
//  `windowSession.scheduleSessionSave()`. The save short-circuits
//  while `isInitializing` is true so didSets that fire during init's
//  seed assignment don't write a ghost empty window. AppState's
//  `start()` calls `markInitializationComplete()` once
//  `restoreSavedWindow()` has populated the tree.
//

import AppKit
import Foundation
import Observation

@MainActor
@Observable
final class WindowSession {
    /// Set of `windowSessionId`s already claimed by live AppStates in
    /// this process. Populated by `restoreSavedWindow` after it picks
    /// a slot. `restoreSavedWindow` consults this to decide whether a
    /// miss-on-match should adopt an unclaimed saved entry (legitimate
    /// first-launch migration) or stay fresh (⌘N opened a second
    /// window; adopting the first window's slot would duplicate pane
    /// ids and defeat per-window isolation).
    private static var claimedWindowIds: Set<String> = []

    /// Stable identifier for this window's entry in `sessions.json`.
    /// Pulled in from `@SceneStorage("windowSessionId")` on
    /// `AppShellView`; survives quits via standard SwiftUI scene
    /// storage so the same window restores the same tab list on
    /// relaunch. Observed so the view layer can mirror adoption
    /// changes back into SceneStorage — `restoreSavedWindow` may
    /// switch us to the bootstrap id, and that re-pairing must
    /// persist.
    private(set) var windowSessionId: String

    /// False in preview/test mode (`services == nil` at AppState
    /// init). Blocks `scheduleSessionSave` so unit tests can't
    /// pollute the real `~/Library/Application Support/Nice/sessions.json`
    /// by exercising the tab-mutation surface.
    @ObservationIgnored
    private let persistenceEnabled: Bool

    /// Blocks `scheduleSessionSave` while AppState's `init` is still
    /// running. Swift fires `activeTabId`'s `didSet` for the seed
    /// assignment in some optional-typed cases, which would otherwise
    /// upsert an empty window entry before `restoreSavedWindow` has
    /// a chance to adopt the bootstrap. Cleared by AppState's
    /// `start()` via `markInitializationComplete()` once the tree is
    /// populated.
    @ObservationIgnored
    private var isInitializing: Bool = true

    @ObservationIgnored
    private weak var tabs: TabModel?
    @ObservationIgnored
    private weak var sessions: SessionsModel?
    @ObservationIgnored
    private weak var sidebar: SidebarModel?

    /// Live `NSWindow` for this scene, set by `AppShellHost`'s
    /// `WindowAccessor` once AppKit hands SwiftUI a real window.
    /// `snapshotPersistedWindow` reads `.frame` from this so each save
    /// captures the current size/position; `restoreSavedWindow`
    /// applies the saved frame after the tab tree is rebuilt. Weak so
    /// teardown doesn't pin the window past its natural lifetime.
    @ObservationIgnored
    weak var window: NSWindow?

    /// Persistence backend. Defaults to `SessionStore.shared` in
    /// production; tests inject a `FakeSessionStore` so they can
    /// assert upsert / prune / flush calls without touching disk.
    @ObservationIgnored
    private let store: SessionStorePersisting

    init(
        tabs: TabModel,
        sessions: SessionsModel,
        sidebar: SidebarModel,
        windowSessionId: String,
        persistenceEnabled: Bool,
        store: SessionStorePersisting = SessionStore.shared
    ) {
        // Brand-new scenes come in with an empty SceneStorage value;
        // mint a UUID here so the scene has a stable id for save/
        // restore from the first body evaluation onward. The view's
        // `onChange(of: windowSessionId)` mirrors this back to
        // SceneStorage so the pairing survives relaunch.
        self.windowSessionId = windowSessionId.isEmpty
            ? UUID().uuidString
            : windowSessionId
        self.persistenceEnabled = persistenceEnabled
        self.tabs = tabs
        self.sessions = sessions
        self.sidebar = sidebar
        self.store = store
    }

    /// AppState calls this at the end of `start()` after
    /// `restoreSavedWindow` has populated the tree, releasing the
    /// save-gate so subsequent mutations persist normally.
    func markInitializationComplete() {
        isInitializing = false
    }

    /// Test-only escape hatch: reset the process-wide
    /// `claimedWindowIds` set so a unit test that constructs multiple
    /// `WindowSession` instances starts each test from a clean slate.
    /// Production never calls this — the only legitimate remover is
    /// `tearDown()` releasing its own id. Internal (visible only via
    /// `@testable import Nice`) so production code can't reach it.
    static func _testing_resetClaimedWindowIds() {
        claimedWindowIds.removeAll()
    }

    /// Test-only readback for the claim set. Used by tests that
    /// assert "second window can adopt the slot after first tears
    /// down" without exposing the storage as a writable seam.
    static func _testing_isClaimed(_ id: String) -> Bool {
        claimedWindowIds.contains(id)
    }

    /// Number of saved windows that no live AppState in this process
    /// has claimed yet. The first-mounted window's `AppShellHost.task`
    /// reads this immediately after `appState.start()` to decide how
    /// many sibling windows to spawn via `openWindow(id: "main")` so
    /// every saved entry in `sessions.json` gets a home on relaunch.
    /// "Non-empty" matches `restoreSavedWindow`'s adoption filter
    /// (`!projects.isEmpty`) — a window whose only project is the
    /// auto-seeded empty Terminals section still reopens as its own
    /// window (the user may have saved that state intentionally).
    static func unclaimedSavedWindowCount(
        store: SessionStorePersisting = SessionStore.shared
    ) -> Int {
        store.load().windows.filter {
            !$0.projects.isEmpty && !claimedWindowIds.contains($0.id)
        }.count
    }

    /// Walk projects for every Claude tab with a `claudeSessionId`,
    /// pack into a `PersistedWindow`, and hand to the debounced
    /// `SessionStore`. Called from every mutation site that changes
    /// the restorable tab set: creation, close, pane-exit dissolve,
    /// auto-title, and active-tab switches. Cheap — the store
    /// coalesces 500ms of rapid updates into a single write.
    func scheduleSessionSave() {
        guard persistenceEnabled, !isInitializing else { return }
        let persisted = snapshotPersistedWindow()
        store.upsert(window: persisted)
    }

    /// Build a `PersistedWindow` from the current model. Mirrors the
    /// sidebar's project grouping so relaunch recreates the same
    /// sidebar structure — in particular, multi-worktree projects
    /// like "NICE" stay a single project. Persists every tab,
    /// including terminal-only tabs in the pinned Terminals group
    /// (they restore with a fresh shell). Empty projects are dropped
    /// except the Terminals project, which is always persisted so
    /// its cwd survives even when every tab was closed.
    /// Internal (not private) so unit tests can assert the
    /// serialization contract without going through disk — projects
    /// round-trip, empty non-terminals projects are dropped, the
    /// Terminals project is always persisted.
    func snapshotPersistedWindow() -> PersistedWindow {
        var persistedProjects: [PersistedProject] = []
        let projects = tabs?.projects ?? []
        for project in projects {
            var persistedTabs: [PersistedTab] = []
            for tab in project.tabs {
                let panes = tab.panes.map {
                    PersistedPane(
                        id: $0.id, title: $0.title, kind: $0.kind, cwd: $0.cwd,
                        // Write nil when false to keep snapshot JSON small —
                        // mirrors `titleManuallySet: tab.titleManuallySet ? true : nil`
                        // a few lines below for `PersistedTab`.
                        titleManuallySet: $0.titleManuallySet ? true : nil
                    )
                }
                persistedTabs.append(PersistedTab(
                    id: tab.id,
                    title: tab.title,
                    cwd: tab.cwd,
                    branch: tab.branch,
                    claudeSessionId: tab.claudeSessionId,
                    activePaneId: tab.activePaneId,
                    panes: panes,
                    titleManuallySet: tab.titleManuallySet ? true : nil,
                    parentTabId: tab.parentTabId,
                    nextTerminalIndex: tab.nextTerminalIndex
                ))
            }
            if persistedTabs.isEmpty && project.id != TabModel.terminalsProjectId {
                continue
            }
            persistedProjects.append(PersistedProject(
                id: project.id,
                name: project.name,
                path: project.path,
                tabs: persistedTabs
            ))
        }
        // Capture the live window's frame so relaunch can restore the
        // user's chosen size/position. nil before the WindowAccessor
        // has fired (very early in scene-graph init) — those rare
        // saves persist with `frame: nil` and the restored window
        // falls back to SwiftUI's default placement.
        let persistedFrame: PersistedFrame? = window.map {
            let f = $0.frame
            return PersistedFrame(
                x: f.origin.x, y: f.origin.y,
                width: f.size.width, height: f.size.height
            )
        }
        return PersistedWindow(
            id: windowSessionId,
            activeTabId: tabs?.activeTabId,
            sidebarCollapsed: sidebar?.sidebarCollapsed ?? false,
            projects: persistedProjects,
            frame: persistedFrame
        )
    }

    /// On AppState.start(): look up this window's saved entry (by
    /// `windowSessionId`) and rebuild its tabs, spawning Claude tabs
    /// with `claude --resume <uuid>` and terminal-only tabs with a
    /// fresh shell. Falls back to adopting an unclaimed entry if
    /// nothing matches this window id — that's how the very first
    /// launch after installing this build picks up the bootstrap file
    /// that was written before `sessions.json` had any live window ids
    /// in it.
    ///
    /// The Claude spawn step is deferred to the next main-queue cycle
    /// so SwiftUI has a chance to mount the new tabs' terminal views
    /// before `startProcess` runs. Claude reads its tty size at
    /// startup and errors out on a 0×0 pty — which is what we got
    /// when the process was spawned synchronously during init, before
    /// the views were ever laid out.
    ///
    /// The pinned Terminals project is guaranteed to exist at index 0
    /// after this runs, regardless of what the snapshot contained.
    func restoreSavedWindow() {
        guard let tabs, let sessions else { return }

        let state = store.load()
        // Try exact match first. If that entry has no projects at all,
        // fall through to the first entry that does — a matched-but-
        // empty slot usually means a prior launch crashed mid-restore;
        // adopting the bootstrap (or whichever window still has state)
        // is the right recovery.
        //
        // If there's no matched slot at all, it's either (a) the first
        // launch of a build where `windowSessionId` semantics changed
        // and the saved state predates it — adopt an unclaimed saved
        // slot as migration, or (b) ⌘N just opened a second window on
        // top of an already-running process — start fresh. Distinguish
        // via the process-wide `claimedWindowIds` set: if some other
        // live AppState already claimed every saved slot we could
        // adopt, we're case (b).
        let matched = state.windows.first(where: { $0.id == windowSessionId })
        let adopted: PersistedWindow?
        if let m = matched, !m.projects.isEmpty {
            adopted = m
        } else if matched != nil {
            // Matched slot exists but is empty — likely a crashed
            // mid-restore. Adopt the first non-empty unclaimed slot.
            adopted = state.windows.first(where: {
                !$0.projects.isEmpty && !Self.claimedWindowIds.contains($0.id)
            })
        } else {
            // No matched slot. Adopt an unclaimed non-empty slot on
            // first-launch migration; stay fresh if every non-empty
            // slot is already owned by another window in this process.
            adopted = state.windows.first(where: {
                !$0.projects.isEmpty && !Self.claimedWindowIds.contains($0.id)
            })
        }

        defer {
            // Claim our slot (either adopted one or our own minted id)
            // so sibling windows spawned next know not to adopt it.
            Self.claimedWindowIds.insert(windowSessionId)
            ensureTerminalsProjectSeededAndSpawn()
        }

        guard let snapshot = adopted else { return }

        // Adopt the entry's id so subsequent saves update that slot
        // instead of creating a duplicate. The view's onChange on
        // `windowSessionId` mirrors the new value back to
        // `@SceneStorage` so the pairing survives relaunch.
        if snapshot.id != windowSessionId {
            windowSessionId = snapshot.id
        }
        // Garbage-collect empty ghost entries left behind by prior
        // launches that failed mid-restore. Keep our newly-adopted
        // slot so scheduleSessionSave has something to upsert into.
        store.pruneEmptyWindows(keeping: snapshot.id)

        // Apply the saved frame before the tab tree is rebuilt so the
        // window lands at its previous size/position before SwiftUI's
        // first layout pass — no visible "open small, then resize"
        // flash. `setFrame` constrains off-screen rects into the
        // current visible screen automatically (handles the
        // disconnected-external-monitor case without us doing the
        // math). nil frame means an older session file without frame
        // persistence; fall back to SwiftUI's default placement.
        if let savedFrame = snapshot.frame, let window = self.window {
            let rect = NSRect(
                x: savedFrame.x, y: savedFrame.y,
                width: savedFrame.width, height: savedFrame.height
            )
            window.setFrame(rect, display: true)
        }

        // Drop any in-init seed from the plain constructor — we want
        // the restored Terminals project (with its own tabs and cwd)
        // to win, not collide with the default one.
        let previousMainTabId = tabs.projects.first(where: { $0.id == TabModel.terminalsProjectId })?.tabs.first?.id
        if let mainTabId = previousMainTabId {
            sessions.terminateAll(tabId: mainTabId)
            sessions.removePtySession(tabId: mainTabId)
        }
        tabs.projects.removeAll()

        // Build the Tab/Pane model now so the sidebar shows the tabs
        // immediately; defer the Claude pty spawn so views can lay out
        // first. Trust the saved project grouping — don't re-bucket by
        // cwd.
        var pendingClaudeSpawns: [(tabId: String, cwd: String, claudePaneId: String?, claudeSessionId: String)] = []
        for persistedProject in snapshot.projects {
            let projectIdx = tabs.ensureProject(
                id: persistedProject.id,
                name: persistedProject.name,
                path: persistedProject.path
            )
            for persistedTab in persistedProject.tabs {
                if let spawn = addRestoredTabModel(
                    persistedTab, toProjectIndex: projectIdx
                ) {
                    pendingClaudeSpawns.append(spawn)
                }
            }
        }

        // Self-heal any drift that pre-dates the git-aware bucketing
        // (mis-bucketed tabs from nested repos, projects rooted at
        // sub-directories of a git repo, duplicates, or empties left
        // behind by repair). Idempotent in steady state, so the cost
        // is just the .git existence checks.
        tabs.repairProjectStructure()
        // Drop /branch lineage pointers whose target tab isn't in the
        // restored tree — a hand-edited or partially-corrupt
        // sessions.json (parent removed by hand, prior crash mid-/branch
        // persisted the child but not the parent) would otherwise
        // render the child indented under nothing. Runs after
        // `repairProjectStructure` because a tab move doesn't change
        // parentTabId, so the same-project invariant — which is the
        // depth-1 contract — is left to materialization-time guards.
        tabs.pruneDanglingParentReferences()
        scheduleSessionSave()

        if let active = snapshot.activeTabId, tabs.tab(for: active) != nil {
            tabs.activeTabId = active
        }

        // Defer Claude spawning until SwiftUI has laid out the
        // terminal views — the pty reads its size at startup. Two
        // main-queue hops: one for SwiftUI's layout pass, one for the
        // terminal view's first setFrameSize.
        //
        // The restored pane is a plain shell with `claude --resume
        // <uuid>` pre-typed at the prompt (see
        // `ClaudeSessionMode.resumeDeferred`). Nothing runs until the
        // user hits Enter, at which point the zsh `claude()` wrapper
        // handshakes with our control socket and gets promoted in
        // place (see `handleClaudeSocketRequest`).
        DispatchQueue.main.async { [weak self] in
            DispatchQueue.main.async { [weak self] in
                guard let self, let sessions = self.sessions else { return }
                for spawn in pendingClaudeSpawns {
                    _ = sessions.makeSession(
                        for: spawn.tabId,
                        cwd: spawn.cwd,
                        extraClaudeArgs: [],
                        initialClaudePaneId: spawn.claudePaneId,
                        initialTerminalPaneId: nil,
                        claudeSessionMode: .resumeDeferred(id: spawn.claudeSessionId)
                    )
                    // If the user was last focused on the terminal
                    // companion (rather than the Claude pane), spawn
                    // it now too so `mainContent` has a pty to render
                    // — otherwise the restored tab opens to a blank
                    // background until the user clicks something.
                    sessions.ensureActivePaneSpawned(tabId: spawn.tabId)
                }
            }
        }
    }

    /// WindowSession-side wrapper around `tabs.ensureTerminalsProjectSeeded(spawnHook:)`
    /// that also spawns a pty for a freshly-synthesized Main tab. The
    /// pure tree-mutation half lives on `TabModel`; the pty side-effect
    /// is bolted on here so the model itself stays process-free.
    private func ensureTerminalsProjectSeededAndSpawn() {
        guard let tabs, let sessions else { return }
        tabs.ensureTerminalsProjectSeeded { [weak sessions] tab in
            _ = sessions?.makeSession(for: tab.id, cwd: tab.cwd)
        }
    }

    // MARK: - Heal-on-restore helpers

    /// Path of Claude Code's per-cwd bucketing root, relative to the
    /// user's home directory. Resolved against `NSHomeDirectory()` at
    /// production call sites so a sandboxed `$HOME` (used by
    /// `TestHomeSandbox` and by Claude itself when running under a
    /// per-launch HOME override) routes the heal scan into the
    /// matching bucket tree rather than the developer's real one.
    static let claudeProjectsDirRelative = "/.claude/projects"

    /// Filename suffix Claude Code uses for per-session transcripts
    /// (one JSON-lines record per message). Centralized so the heal
    /// scan and any future indexer touch one constant if Claude
    /// changes the suffix.
    static let transcriptExtension = ".jsonl"

    /// How many transcript lines `readCwdFromTranscript` will scan
    /// before giving up. Sized to cover Claude's transcript head —
    /// permission-mode + (worktree-state) + file-history-snapshot +
    /// the first user/attachment/assistant records — without
    /// ballooning the per-restore I/O cost for sessions whose head
    /// records have no cwd field for some reason. Internal so the
    /// pure-helper tests can pin the boundary instead of hard-coding
    /// the number.
    static let transcriptHeadScanLines = 30

    /// Locate a Claude session's actual on-disk bucket when the
    /// persisted `tab.cwd` doesn't match what Claude bucketed under.
    /// Returns the recovered cwd (suitable for both `tab.cwd`
    /// persistence and as the deferred-shell spawn directory) or nil
    /// when no heal is necessary (transcript already present at the
    /// expected path) or no heal is possible (session id absent from
    /// every bucket, transcript unreadable, recovered path no longer
    /// exists on disk).
    ///
    /// How the bug shape arises (history): bare `claude -w` (no name)
    /// auto-generates a worktree name Nice can't predict at the args
    /// layer, so `createTabFromMainTerminal`'s
    /// `extractWorktreeName`-based detection misses and `tab.cwd`
    /// records the pre-worktree project path. The SessionStart hook
    /// now forwards Claude's real cwd back so new sessions stay in
    /// sync, but existing on-disk state from before that fix can only
    /// recover via this restore-time scan.
    ///
    /// `projectsRoot` defaults to the production-shape path under
    /// `$HOME`; tests pass a temp-dir override so direct unit tests
    /// of the heal helpers don't need `TestHomeSandbox`. Static so it
    /// can be called from `addRestoredTabModel` without allocating.
    static func healSpawnCwd(
        sessionId: String,
        persistedCwd: String,
        projectsRoot: String = defaultClaudeProjectsRoot()
    ) -> String? {
        let expectedBucket = encodeClaudeBucket(
            TabModel.expandTilde(persistedCwd)
        )
        let expectedTranscript =
            "\(projectsRoot)/\(expectedBucket)/\(sessionId)\(transcriptExtension)"
        if FileManager.default.fileExists(atPath: expectedTranscript) {
            return nil
        }

        // Enumerate every sibling bucket for `<sessionId>.jsonl`.
        // contentsOfDirectory returns an empty array (not throws) on
        // a missing or unreadable projects dir on macOS, but `try?`
        // covers the throw-path defensively.
        guard let buckets = try? FileManager.default.contentsOfDirectory(
            atPath: projectsRoot
        ), !buckets.isEmpty else { return nil }

        var matches: [(path: String, mtime: Date)] = []
        for bucket in buckets {
            let candidate =
                "\(projectsRoot)/\(bucket)/\(sessionId)\(transcriptExtension)"
            guard let attrs = try? FileManager.default.attributesOfItem(
                atPath: candidate
            ) else { continue }
            let mtime = (attrs[.modificationDate] as? Date) ?? .distantPast
            matches.append((candidate, mtime))
        }
        guard let chosen = matches
            .sorted(by: { $0.mtime > $1.mtime })
            .first?.path
        else {
            // No diagnostic log here: this branch fires whenever a
            // restored Claude tab's transcript isn't recoverable from
            // any bucket — common in tests that don't plant
            // transcripts, and indistinguishable in production from
            // legitimately-deleted sessions. The downstream "Claude
            // couldn't find session X" error in the deferred-resume
            // pane is the user-facing signal; an NSLog here would
            // just spam stderr on every launch where any tab's
            // session was cleaned up server-side.
            return nil
        }

        guard let recovered = readCwdFromTranscript(at: chosen) else {
            NSLog("WindowSession.healSpawnCwd: transcript at \(chosen) yielded no cwd in the first \(transcriptHeadScanLines) lines")
            return nil
        }
        // The recovered cwd is what Claude expects on resume, but
        // there's no point rewriting `tab.cwd` to a phantom path —
        // if the worktree was deleted between sessions, the resume is
        // unrecoverable either way, and `resolvedSpawnCwd`'s existing
        // fallback will still drop the user into the project root.
        let expanded = TabModel.expandTilde(recovered)
        guard FileManager.default.fileExists(atPath: expanded) else {
            NSLog("WindowSession.healSpawnCwd: recovered cwd \(recovered) for session \(sessionId) does not exist on disk; abandoning heal")
            return nil
        }
        return recovered
    }

    /// Production-shape projects root: Claude Code's bucketing tree
    /// under the current user's home. Factored so tests have a single
    /// well-named constant to override and the default-argument
    /// expression on `healSpawnCwd` stays one identifier wide.
    static func defaultClaudeProjectsRoot() -> String {
        NSHomeDirectory() + claudeProjectsDirRelative
    }

    /// Mirror of Claude Code's bucket-name convention: replace every
    /// `/` and `.` in the absolute path with `-`. Observed in
    /// `~/.claude/projects/` (e.g. `/Users/nick/Projects/notes/.claude/worktrees/foo`
    /// → `-Users-nick-Projects-notes--claude-worktrees-foo`).
    /// The encoding is lossy in general (two distinct paths can map to
    /// the same bucket), which is precisely why
    /// `readCwdFromTranscript` pulls the real path out of the file
    /// content rather than trying to decode the bucket name.
    static func encodeClaudeBucket(_ cwd: String) -> String {
        var result = ""
        result.reserveCapacity(cwd.count)
        for ch in cwd {
            if ch == "/" || ch == "." {
                result.append("-")
            } else {
                result.append(ch)
            }
        }
        return result
    }

    /// Read the first `transcriptHeadScanLines` newline-delimited
    /// JSON records of a Claude transcript and return the first cwd
    /// value found. Per-message records carry a top-level `"cwd"`
    /// field; worktree sessions also emit a `{"type":"worktree-state",
    /// "worktreeSession": {"worktreePath": "..."}}` record near the
    /// top, used as a fallback for transcripts whose head doesn't yet
    /// include a regular message with `cwd`. Returns nil if the file
    /// is missing, unreadable, non-UTF-8, or the scanned head
    /// contains neither field — caller falls back to the existing
    /// `resolvedSpawnCwd` behavior in that case.
    static func readCwdFromTranscript(at path: String) -> String? {
        guard let content = try? String(
            contentsOfFile: path, encoding: .utf8
        ) else {
            NSLog("WindowSession.readCwdFromTranscript: unable to read \(path)")
            return nil
        }
        for line in content.split(
            separator: "\n", omittingEmptySubsequences: true
        ).prefix(transcriptHeadScanLines) {
            guard let data = line.data(using: .utf8),
                  let obj = try? JSONSerialization.jsonObject(with: data)
                  as? [String: Any]
            else { continue }
            if let cwd = obj["cwd"] as? String, !cwd.isEmpty {
                return cwd
            }
            if let nested = obj["worktreeSession"] as? [String: Any],
               let worktreePath = nested["worktreePath"] as? String,
               !worktreePath.isEmpty
            {
                return worktreePath
            }
        }
        return nil
    }

    /// Append one restored tab's model to `tabs.projects[projectIndex]`.
    /// Claude tabs (tabs with a `claudeSessionId`) return info so the
    /// caller can defer the pty spawn to `claude --resume`. Terminal-
    /// only tabs spawn their shell eagerly and return nil.
    ///
    /// Internal (not private) so tests can assert (via the AppState
    /// forwarder) that the returned spawn cwd falls back from a
    /// missing worktree to the project path.
    func addRestoredTabModel(
        _ persisted: PersistedTab,
        toProjectIndex projectIndex: Int
    ) -> (tabId: String, cwd: String, claudePaneId: String?, claudeSessionId: String)? {
        guard let tabs, let sessions else { return nil }

        let panes = persisted.panes.map { pp in
            // Hydrate `nil → false` for v3 session files written before
            // the flag existed.
            Pane(
                id: pp.id, title: pp.title, kind: pp.kind, cwd: pp.cwd,
                titleManuallySet: pp.titleManuallySet ?? false
            )
        }
        let defaultActive = panes.first(where: { $0.kind == .claude })?.id
            ?? panes.first?.id

        // Hydrate the monotonic terminal-index counter. Older session
        // files lack the persisted value; recover it from the pane
        // titles via the model-side helper so the regex grammar lives
        // in one place.
        let hydratedNextTerminalIndex = persisted.nextTerminalIndex
            ?? Tab.recoverNextTerminalIndex(
                fromPaneTitles: persisted.panes.map(\.title)
            )

        var tab = Tab(
            id: persisted.id,
            title: persisted.title,
            cwd: persisted.cwd,
            branch: persisted.branch,
            panes: panes,
            activePaneId: persisted.activePaneId ?? defaultActive,
            titleAutoGenerated: persisted.claudeSessionId != nil,
            titleManuallySet: persisted.titleManuallySet ?? false,
            claudeSessionId: persisted.claudeSessionId,
            parentTabId: persisted.parentTabId
        )
        tab.nextTerminalIndex = hydratedNextTerminalIndex

        tabs.projects[projectIndex].tabs.append(tab)

        // Resolve after appending so `resolvedSpawnCwd` can see the tab's
        // new project context. Falls back to the project path if the
        // persisted cwd (e.g. a worktree directory) has been deleted
        // since the last launch.
        var spawnCwd = tabs.resolvedSpawnCwd(for: tab)

        // Heal-on-restore: pre-hook-cwd-forwarding builds (or any
        // earlier code path that wrote a stale `tab.cwd`) can leave a
        // Claude tab pointing at the project root while the real
        // transcript lives in `~/.claude/projects/<encoded-worktree>/`.
        // Resume from the wrong bucket fails with "No conversation
        // found", so before returning the spawn cwd, check whether the
        // expected transcript actually exists. If it doesn't, locate
        // the transcript by session id across every projects bucket,
        // recover the real cwd from the file's content, and adopt it
        // — both for this spawn and for the persisted tab so the
        // correction lands on disk via the upcoming save flush.
        if let sid = persisted.claudeSessionId,
           let healed = Self.healSpawnCwd(
            sessionId: sid, persistedCwd: spawnCwd
           )
        {
            if tabs.adoptTabCwd(forTabId: tab.id, newCwd: healed) {
                scheduleSessionSave()
            }
            spawnCwd = healed
        }

        if let sid = persisted.claudeSessionId {
            let claudePaneId = panes.first(where: { $0.kind == .claude })?.id
            return (
                tabId: tab.id,
                cwd: spawnCwd,
                claudePaneId: claudePaneId,
                claudeSessionId: sid
            )
        }

        // Terminal-only tab: bring its active pane's shell up now at
        // that pane's last-observed cwd. We honour `activePaneId`
        // instead of letting `makeSession` infer the first terminal —
        // if the user quit while focused on a secondary pane,
        // spawning the first instead leaves the active pane unhosted
        // and `mainContent` renders the blank fallback. Per-pane
        // cwd falls back to the tab cwd when the pane has none
        // persisted, or its cwd no longer exists. Other panes stay
        // lazy until first focus.
        let activeTerminal = tab.activePaneId.flatMap { id in
            tab.panes.first(where: { $0.id == id && $0.kind == .terminal })
        } ?? tab.panes.first(where: { $0.kind == .terminal })
        if let pane = activeTerminal {
            let paneCwd = tabs.resolvedSpawnCwd(for: tab, pane: pane)
            _ = sessions.makeSession(
                for: tab.id,
                cwd: paneCwd,
                initialTerminalPaneId: pane.id
            )
        } else {
            _ = sessions.makeSession(for: tab.id, cwd: spawnCwd)
        }
        return nil
    }

    /// Why this window is going away. The two close paths must
    /// diverge on disk: a window the user explicitly closed should
    /// not reappear on next launch, while a window that happened to
    /// be open at quit time should.
    ///
    /// Caller routing — both signals exist and must be wired
    /// correctly:
    ///
    /// - `WindowRegistry.handleClose` (the `willCloseNotification`
    ///   path) dispatches `.userClosedWindow` only when
    ///   `AppState.userInitiatedClose` is true. AppKit only flips
    ///   that flag via `windowShouldClose` (red traffic light /
    ///   ⌘W), so it's an unambiguous "user wants this gone."
    ///
    /// - `NiceServices`'s `willTerminate` observer dispatches
    ///   `.appTerminating` for every still-registered AppState.
    ///   This observer also calls
    ///   `WindowRegistry.detachAllCloseObservers()` first so the
    ///   subsequent SwiftUI scene-teardown burst (which posts
    ///   `willCloseNotification` for every live window during
    ///   `app.terminate(_:)`) cannot retroactively reach
    ///   `handleClose`. Belt-and-suspenders against an earlier
    ///   regression where this very class assumed (wrongly) that
    ///   `willCloseNotification` doesn't fire during terminate.
    enum TearDownReason {
        /// User explicitly closed the window via the red traffic
        /// light / ⌘W. Drop the entry from `sessions.json` so it
        /// doesn't reopen.
        case userClosedWindow
        /// App is terminating with this window still open (or the
        /// user-close path completed without setting the intent
        /// flag — defaults to preserving the snapshot, which is
        /// the safer failure mode). Persist the latest snapshot so
        /// relaunch reopens it.
        case appTerminating
    }

    /// Finalize this window's persistence and release its claim on
    /// `claimedWindowIds`. Called from `AppState.tearDown(reason:)`
    /// before the pty subsystem comes down, so the final model
    /// state (including auto-titles that arrived mid-session) makes
    /// it to disk on the `.appTerminating` path. Skipped in
    /// preview/test mode so tests can't pollute the real
    /// sessions.json.
    func tearDown(reason: TearDownReason) {
        if persistenceEnabled {
            switch reason {
            case .userClosedWindow:
                store.remove(windowId: windowSessionId)
            case .appTerminating:
                store.upsert(window: snapshotPersistedWindow())
            }
            store.flush()
        }
        // Release the session-id claim regardless of reason —
        // neither path should leave a stale id pinning future
        // windows in this process out of the slot. (For
        // .userClosedWindow the slot is also gone from disk, but
        // that's the store's concern, not the in-process claim
        // set's.)
        Self.claimedWindowIds.remove(windowSessionId)
    }
}
