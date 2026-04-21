//
//  SessionStore.swift
//  Nice
//
//  Persists the list of live tabs per window to
//  `~/Library/Application Support/Nice/sessions.json` so a relaunch
//  can pick up where the user left off. Claude tabs resume via
//  `claude --resume <uuid>`; terminal-only tabs (including every tab
//  in the pinned Terminals group) restore with a fresh shell.
//
//  Window identity is preserved across quits via
//  `@SceneStorage("windowSessionId")` on `AppShellView`.
//
//  Writes are debounced (500ms) so rapid-fire state mutations during
//  normal use don't thrash the disk. `flush()` cancels the pending
//  timer and writes synchronously; `willTerminate` calls it through
//  every `AppState.tearDown` so the last state makes it to disk
//  before the app exits.
//

import Foundation

struct PersistedPane: Codable, Hashable, Sendable {
    let id: String
    let title: String
    let kind: PaneKind
}

struct PersistedTab: Codable, Hashable, Sendable {
    let id: String
    let title: String
    let cwd: String
    let branch: String?
    /// Non-nil for Claude tabs (used for `claude --resume <uuid>` on
    /// restore). Nil for terminal-only tabs — these come back as a
    /// fresh shell in the persisted cwd.
    let claudeSessionId: String?
    let activePaneId: String?
    let panes: [PersistedPane]
    /// Whether the user renamed this tab via the sidebar inline
    /// editor. Optional so v3 session files (written before this flag
    /// existed) still decode — callers hydrate with `?? false`.
    let titleManuallySet: Bool?

    init(
        id: String,
        title: String,
        cwd: String,
        branch: String?,
        claudeSessionId: String?,
        activePaneId: String?,
        panes: [PersistedPane],
        titleManuallySet: Bool? = nil
    ) {
        self.id = id
        self.title = title
        self.cwd = cwd
        self.branch = branch
        self.claudeSessionId = claudeSessionId
        self.activePaneId = activePaneId
        self.panes = panes
        self.titleManuallySet = titleManuallySet
    }
}

/// Sidebar project grouping. Preserves the name/path the user actually
/// saw in the sidebar across relaunches — deriving it fresh from each
/// tab's cwd on restore would split a multi-worktree project like
/// "NICE" into one per worktree dir (no common cwd prefix between
/// worktrees).
struct PersistedProject: Codable, Hashable, Sendable {
    let id: String
    let name: String
    let path: String
    let tabs: [PersistedTab]
}

struct PersistedWindow: Codable, Hashable, Sendable {
    let id: String
    let activeTabId: String?
    let sidebarCollapsed: Bool
    let projects: [PersistedProject]
}

extension PersistedWindow {
    /// Total saved tabs across all projects in this window. Used by
    /// callers that want to know "does this window actually have any
    /// restorable state" without caring which project owns what.
    var totalTabCount: Int {
        projects.reduce(0) { $0 + $1.tabs.count }
    }
}

struct PersistedState: Codable, Hashable, Sendable {
    let version: Int
    let windows: [PersistedWindow]

    /// Bumped to 3 when the pinned Terminals group started persisting
    /// terminal-only tabs (making `PersistedTab.claudeSessionId`
    /// optional and dropping the `mainTerminalCwd` window-level
    /// field). v1/v2 files fail to decode and we start fresh; users
    /// with older sessions.json lose saved tabs on first launch of
    /// this build. Acceptable one-off migration, mirroring the v1→v2
    /// bump.
    static let currentVersion = 3
    static let empty = PersistedState(version: currentVersion, windows: [])
}

@MainActor
final class SessionStore {
    static let shared = SessionStore()

    private let fileURL: URL
    private var pendingWork: DispatchWorkItem?
    /// Debounce window for `upsert`. Short enough that a quick ⌘W +
    /// ⌘Q still catches the final state (the subsequent `flush` in
    /// `tearDown` runs synchronously and cancels the pending work).
    private let debounceInterval: TimeInterval = 0.5

    /// In-memory cache of the last-read / last-written state. Keeps
    /// `upsert` cheap: it mutates the cached struct and schedules a
    /// single write instead of re-reading the file on every change.
    private var cached: PersistedState

    init() {
        let fm = FileManager.default
        // Tests set `NICE_APPLICATION_SUPPORT_ROOT` to redirect session
        // state into a sandbox directory. `FileManager.url(for:
        // .applicationSupportDirectory, ...)` resolves the user's home
        // via `getpwuid(getuid())` and ignores `$HOME`, so overriding
        // HOME alone isn't enough to keep UITests from reading and
        // mutating the user's real `~/Library/Application Support/Nice/
        // sessions.json`. Production leaves this unset.
        let root: URL
        if let override = ProcessInfo.processInfo.environment["NICE_APPLICATION_SUPPORT_ROOT"],
           !override.isEmpty {
            root = URL(fileURLWithPath: override, isDirectory: true)
        } else {
            // `create: true` already creates the directory; we still
            // append the folder name ourselves and create that subdir
            // below.
            root = (try? fm.url(
                for: .applicationSupportDirectory,
                in: .userDomainMask,
                appropriateFor: nil,
                create: true
            )) ?? URL(fileURLWithPath: NSHomeDirectory())
                .appendingPathComponent("Library/Application Support", isDirectory: true)
        }
        // Folder name tracks CFBundleName so the `Nice Dev` variant
        // lands at `~/Library/Application Support/Nice Dev/` and can't
        // clobber the user's real sessions in `…/Nice/`.
        let folder = (Bundle.main.object(forInfoDictionaryKey: "CFBundleName") as? String) ?? "Nice"
        let supportDir = root.appendingPathComponent(folder, isDirectory: true)
        try? fm.createDirectory(at: supportDir, withIntermediateDirectories: true)
        self.fileURL = supportDir.appendingPathComponent("sessions.json")
        self.cached = Self.read(from: fileURL)
    }

    /// Return the current persisted state. Safe to call at any time —
    /// returns the cached value without hitting disk after the first
    /// read.
    func load() -> PersistedState {
        return cached
    }

    /// Merge `window` into the persisted state, replacing any existing
    /// entry with the same id. Schedules a debounced write; callers
    /// don't need to block on I/O.
    func upsert(window: PersistedWindow) {
        var windows = cached.windows
        if let idx = windows.firstIndex(where: { $0.id == window.id }) {
            windows[idx] = window
        } else {
            windows.append(window)
        }
        cached = PersistedState(version: PersistedState.currentVersion, windows: windows)
        scheduleWrite()
    }

    /// Drop every window with zero tabs except `keep` (usually the
    /// window id of the caller so they can still save into their
    /// slot). Called from `AppState.restoreSavedWindow` to garbage-
    /// collect "ghost" entries accumulated by prior launches whose
    /// restore attempts all failed.
    func pruneEmptyWindows(keeping keep: String) {
        let filtered = cached.windows.filter { $0.id == keep || $0.totalTabCount > 0 }
        guard filtered.count != cached.windows.count else { return }
        cached = PersistedState(version: PersistedState.currentVersion, windows: filtered)
        scheduleWrite()
    }

    /// Cancel any pending debounced write and flush the cache to disk
    /// synchronously. Called from `AppState.tearDown` so `willTerminate`
    /// never loses the last mutation.
    func flush() {
        pendingWork?.cancel()
        pendingWork = nil
        Self.write(cached, to: fileURL)
    }

    // MARK: - Disk I/O

    private func scheduleWrite() {
        pendingWork?.cancel()
        let snapshot = cached
        let url = fileURL
        let work = DispatchWorkItem {
            Self.write(snapshot, to: url)
        }
        pendingWork = work
        DispatchQueue.main.asyncAfter(
            deadline: .now() + debounceInterval, execute: work
        )
    }

    private static func read(from url: URL) -> PersistedState {
        guard let data = try? Data(contentsOf: url) else { return .empty }
        let decoder = JSONDecoder()
        guard let decoded = try? decoder.decode(PersistedState.self, from: data) else {
            return .empty
        }
        return decoded
    }

    private static func write(_ state: PersistedState, to url: URL) {
        let encoder = JSONEncoder()
        // `.withoutEscapingSlashes` keeps path values readable
        // (`/Users/...` instead of `\/Users\/...`). Both forms
        // decode to the same string, so this is purely cosmetic.
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        guard let data = try? encoder.encode(state) else { return }
        try? data.write(to: url, options: .atomic)
    }
}
