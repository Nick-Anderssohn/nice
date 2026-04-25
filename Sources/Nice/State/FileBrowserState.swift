//
//  FileBrowserState.swift
//  Nice
//
//  Per-tab state for the sidebar's file-browser mode. Stored in
//  `AppState.fileBrowserStates` keyed by `Tab.id`, seeded from the
//  tab's `cwd` on first access. Lives only in memory: expansion sets
//  and scroll positions don't roundtrip well across launches because
//  the directories may have churned, and the tab's CWD is already
//  persisted on `Tab` itself.
//

import Foundation
import SwiftUI

@MainActor
final class FileBrowserState: ObservableObject {
    /// Absolute path of the directory currently shown as the tree root.
    /// Seeded from the owning tab's CWD; mutated by the breadcrumb
    /// up-arrow, the project-name header (resets to CWD), the empty-
    /// state "Go to CWD" button, and double-clicking a folder.
    ///
    /// `didSet` adds the new root to `expandedPaths` so the tree
    /// shows its contents by default after a re-root. Users can
    /// still collapse the root manually — the row is treated like
    /// any other directory, no `isRoot` exception in the view.
    @Published var rootPath: String {
        didSet {
            expandedPaths.insert(rootPath)
        }
    }

    /// Absolute paths of directories the user has expanded. The tree
    /// row for a directory shows its children iff its path is in this
    /// set. Stays accurate across rebuilds because we key on absolute
    /// paths, not on identity. The current `rootPath` is always in
    /// this set when freshly seeded / re-rooted; the user can still
    /// remove it by clicking the disclosure triangle to collapse.
    @Published var expandedPaths: Set<String> = []

    /// Whether dotfiles are visible. The seed value is CWD-aware (see
    /// `defaultShowHidden(forCwd:)` — hidden in $HOME so the user's
    /// home isn't overwhelmed with config dotfiles, visible elsewhere
    /// because dotfiles in a project root are usually relevant
    /// content). Once seeded, `showHidden` is sticky against
    /// breadcrumb navigation — the user's explicit toggle persists
    /// even after they navigate into or out of $HOME.
    @Published var showHidden: Bool

    init(rootPath: String) {
        self.rootPath = rootPath
        self.showHidden = Self.defaultShowHidden(forCwd: rootPath)
        // Seed the root expanded so files show on first render.
        // `didSet` doesn't fire from `init`, so do this explicitly.
        self.expandedPaths.insert(rootPath)
    }

    /// Seed value for `showHidden` based on the spawning CWD. Hidden
    /// off when the CWD is exactly the user's home directory (the
    /// dotfile flood there isn't useful default content); hidden on
    /// everywhere else (a project root's `.gitignore`, `.env` etc.
    /// are content the developer expects to see).
    ///
    /// Comparison normalizes via `URL.standardizedFileURL` so
    /// trailing slashes and `~` expansion don't cause false negatives.
    static func defaultShowHidden(forCwd cwd: String) -> Bool {
        let expanded = (cwd as NSString).expandingTildeInPath
        let cwdURL = URL(fileURLWithPath: expanded).standardizedFileURL
        let homeURL = URL(fileURLWithPath: NSHomeDirectory()).standardizedFileURL
        return cwdURL.path != homeURL.path
    }

    func toggleExpansion(of path: String) {
        if expandedPaths.contains(path) {
            expandedPaths.remove(path)
        } else {
            expandedPaths.insert(path)
        }
    }
}

/// Watches a single directory for entry add / remove / rename events
/// using a kqueue-backed `DispatchSource`. The handler is debounced
/// (~120ms) so a burst of writes from a single editor save fires one
/// reload, not a flurry. One open file descriptor per watched directory
/// — with realistic usage (a few tabs × a few expanded folders) we
/// stay well under the per-process FD limit.
///
/// The `FileTreeRow` views in `FileBrowserView` each own a watcher and
/// drive its lifecycle from `.onAppear` / `.onChange(of: isExpanded)` /
/// `.onDisappear`, so collapsed and offscreen directories don't hold
/// FDs.
@MainActor
final class DirectoryWatcher: ObservableObject {
    private var source: DispatchSourceFileSystemObject?
    private var fd: Int32 = -1
    private var pendingWork: DispatchWorkItem?

    /// Begin watching `path`. Calling `start` again replaces any prior
    /// watch; safe to call without an explicit `stop` first.
    func start(path: String, onChange: @escaping () -> Void) {
        stop()
        let fd = open(path, O_EVTONLY)
        guard fd >= 0 else { return }
        self.fd = fd
        let src = DispatchSource.makeFileSystemObjectSource(
            fileDescriptor: fd,
            eventMask: [.write, .delete, .rename, .extend],
            queue: .main
        )
        src.setEventHandler { [weak self] in
            self?.scheduleDebounced(onChange)
        }
        src.setCancelHandler { [fd] in
            close(fd)
        }
        src.resume()
        source = src
    }

    func stop() {
        pendingWork?.cancel()
        pendingWork = nil
        source?.cancel()
        source = nil
        fd = -1
    }

    private func scheduleDebounced(_ onChange: @escaping () -> Void) {
        pendingWork?.cancel()
        let work = DispatchWorkItem { onChange() }
        pendingWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.12, execute: work)
    }

    deinit {
        // Dispatch sources are safe to cancel from any context;
        // their cancel handler closes the FD.
        source?.cancel()
    }
}
