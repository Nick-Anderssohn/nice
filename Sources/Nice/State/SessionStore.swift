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
//  normal use don't thrash the disk, and the encode + atomic write
//  always runs on a private serial `ioQueue` so main is never the
//  writer. `flush()` cancels the pending timer and blocks the caller
//  on a semaphore until the off-main write completes; per-window-close
//  and `willTerminate` both reach `flush()` through `WindowSession.tearDown`,
//  so the last state makes it to disk before the close/exit returns.
//
//  Disk I/O is funneled through `SessionStoreIO` (production:
//  `DiskSessionStoreIO`). Tests inject a recorder to capture writes
//  with queue/thread context — the protocol is the only path from
//  `SessionStore` to the filesystem.
//

import Foundation

struct PersistedPane: Codable, Hashable, Sendable {
    let id: String
    let title: String
    let kind: PaneKind
    /// Last-observed cwd for this pane, captured from OSC 7 in the
    /// injected zsh `chpwd_functions` hook. Optional so v3 session
    /// files (written before per-pane cwd existed) still decode —
    /// restore falls back to `PersistedTab.cwd` when nil.
    let cwd: String?

    init(id: String, title: String, kind: PaneKind, cwd: String? = nil) {
        self.id = id
        self.title = title
        self.kind = kind
        self.cwd = cwd
    }
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
    /// Monotonic per-tab counter for auto-naming new terminal panes
    /// "Terminal N". Optional so older session files still decode —
    /// `WindowSession.addRestoredTabModel` recomputes the counter from
    /// existing pane titles when this is nil.
    let nextTerminalIndex: Int?

    init(
        id: String,
        title: String,
        cwd: String,
        branch: String?,
        claudeSessionId: String?,
        activePaneId: String?,
        panes: [PersistedPane],
        titleManuallySet: Bool? = nil,
        nextTerminalIndex: Int? = nil
    ) {
        self.id = id
        self.title = title
        self.cwd = cwd
        self.branch = branch
        self.claudeSessionId = claudeSessionId
        self.activePaneId = activePaneId
        self.panes = panes
        self.titleManuallySet = titleManuallySet
        self.nextTerminalIndex = nextTerminalIndex
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

/// Surface used by `WindowSession` for persistence — a protocol so
/// unit tests can swap in `FakeSessionStore` to capture upserts /
/// flushes without touching `~/Library/Application Support/Nice/`.
/// `SessionStore.shared` is the only production implementer.
@MainActor
protocol SessionStorePersisting: AnyObject {
    func load() -> PersistedState
    func upsert(window: PersistedWindow)
    func pruneEmptyWindows(keeping: String)
    /// Cancel any pending debounced write and persist the current
    /// state. Conformers must block until the write completes so
    /// callers (`willTerminate`, per-window-close) can rely on the
    /// state being on disk before the call returns.
    func flush()
}

/// Disk side-effects performed by `SessionStore`. Carved out as a
/// protocol so tests can inject a recorder that captures writes
/// (with queue/thread context) and skip touching disk, and so the
/// only path from `SessionStore` to the filesystem is one method
/// call away from the I/O queue. Production uses
/// `DiskSessionStoreIO`.
protocol SessionStoreIO: Sendable {
    /// Encode `state` and write it to `url` atomically. Always
    /// invoked on `SessionStore`'s I/O queue — implementations must
    /// be safe to call from a background thread.
    func write(_ state: PersistedState, to url: URL)
    /// Synchronously read the persisted state at `url`, returning
    /// `.empty` if the file is missing or fails to decode. Called
    /// once at `SessionStore.init`.
    func read(from url: URL) -> PersistedState
}

/// Production `SessionStoreIO`: JSON-encode → atomic write to disk
/// with `try?`. A failed atomic write leaves the prior file intact,
/// which is the recovery contract pinned by
/// `SessionStoreWriteFailureTests`.
struct DiskSessionStoreIO: SessionStoreIO {
    func write(_ state: PersistedState, to url: URL) {
        let encoder = JSONEncoder()
        // `.withoutEscapingSlashes` keeps path values readable
        // (`/Users/...` instead of `\/Users\/...`). Both forms
        // decode to the same string, so this is purely cosmetic.
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        guard let data = try? encoder.encode(state) else { return }
        try? data.write(to: url, options: .atomic)
    }

    func read(from url: URL) -> PersistedState {
        guard let data = try? Data(contentsOf: url) else { return .empty }
        guard let decoded = try? JSONDecoder().decode(PersistedState.self, from: data) else {
            return .empty
        }
        return decoded
    }
}

@MainActor
final class SessionStore: SessionStorePersisting {
    /// Label of the serial queue that owns every `SessionStore` write.
    /// Exposed (internal) so tests can assert "this write ran on
    /// `ioQueue`, not main, not some other queue" via the dispatch
    /// queue label C API.
    static let ioQueueLabel = "dev.nickanderssohn.nice.sessionstore.io"

    static let shared = SessionStore()

    private let fileURL: URL
    private var pendingWork: DispatchWorkItem?
    /// Debounce window for `upsert`. Short enough that a quick ⌘W +
    /// ⌘Q still catches the final state (the subsequent `flush` in
    /// `tearDown` runs synchronously and cancels the pending work).
    private let debounceInterval: TimeInterval = 0.5

    /// Serial background queue for the encode + atomic file write.
    /// Keeps disk I/O off the main thread; serial so a flush write
    /// always runs after any earlier debounced write that already
    /// left the main-thread timer. Note: `qos: .utility` plus a
    /// main-thread `sem.wait()` in `flush()` is technically a
    /// priority inversion, but macOS auto-promotes the queue's QoS
    /// for the duration of the wait, so the wait isn't starved.
    private let ioQueue = DispatchQueue(
        label: SessionStore.ioQueueLabel,
        qos: .utility
    )

    /// Disk-side effects. Defaults to `DiskSessionStoreIO` in
    /// production; tests inject a recorder so they can assert on
    /// writes (count, ordering, queue) without touching disk.
    private let io: SessionStoreIO

    /// In-memory cache of the last-read / last-written state. Keeps
    /// `upsert` cheap: it mutates the cached struct and schedules a
    /// single write instead of re-reading the file on every change.
    private var cached: PersistedState

    init(io: SessionStoreIO = DiskSessionStoreIO()) {
        self.io = io
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
        self.cached = io.read(from: fileURL)
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

    /// Cancel any pending debounced write and flush the cache to disk.
    /// The write itself runs on `ioQueue` (so main is never the
    /// writer); the semaphore makes this call block until the write
    /// completes, preserving the "flushed before terminate returns"
    /// guarantee. Called from `WindowSession.tearDown` on per-window
    /// close *and* on app terminate.
    func flush() {
        dispatchPrecondition(condition: .onQueue(.main))
        pendingWork?.cancel()
        pendingWork = nil
        enqueueWriteAndWait(snapshot: cached)
    }

    // MARK: - Disk I/O

    private func scheduleWrite() {
        dispatchPrecondition(condition: .onQueue(.main))
        pendingWork?.cancel()
        let snapshot = cached
        // Timer still lives on main so cancel-on-rescheduling stays
        // race-free with `pendingWork` (a `@MainActor`-isolated ivar).
        // When it fires, it hands the snapshot to `enqueueWrite`; the
        // actual encode + atomic write runs on the background worker.
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated {
                self?.enqueueWrite(snapshot: snapshot)
            }
        }
        pendingWork = work
        DispatchQueue.main.asyncAfter(
            deadline: .now() + debounceInterval, execute: work
        )
    }

    /// Single funnel for non-blocking writes. Captures `io` and `url`
    /// as values, then dispatches to `ioQueue`. The
    /// `dispatchPrecondition` inside the block is the structural guard
    /// that no future caller bypasses the I/O queue: every write goes
    /// through here.
    private func enqueueWrite(snapshot: PersistedState) {
        let io = self.io
        let url = self.fileURL
        ioQueue.async { [ioQueue] in
            dispatchPrecondition(condition: .onQueue(ioQueue))
            io.write(snapshot, to: url)
        }
    }

    /// Blocking variant for `flush`. Same enqueue path, plus a
    /// semaphore so the caller can rely on the write being on disk
    /// before the call returns.
    private func enqueueWriteAndWait(snapshot: PersistedState) {
        let io = self.io
        let url = self.fileURL
        let sem = DispatchSemaphore(value: 0)
        ioQueue.async { [ioQueue] in
            dispatchPrecondition(condition: .onQueue(ioQueue))
            io.write(snapshot, to: url)
            sem.signal()
        }
        sem.wait()
    }
}
