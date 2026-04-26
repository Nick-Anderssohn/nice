//
//  FileBrowserView.swift
//  Nice
//
//  Sidebar content for `SidebarMode.files`. Renders a recursive
//  disclosure tree rooted at the active tab's CWD, with a breadcrumb
//  row at the top for up-nav, refresh, and a hidden-files toggle.
//
//  Per-tab state (root path, expanded set, hidden visibility) lives
//  on `AppState.fileBrowserStates`; this view reads it and re-rebinds
//  whenever `appState.activeTabId` changes so each tab's tree is
//  preserved when the user switches away and back.
//

import AppKit
import SwiftUI
import UniformTypeIdentifiers

struct FileBrowserView: View {
    @EnvironmentObject private var appState: AppState
    @EnvironmentObject private var fontSettings: FontSettings
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    var body: some View {
        // Re-derive every time activeTabId / its CWD changes so the
        // browser snaps to whichever tab the user is now viewing,
        // pulling that tab's preserved state from AppState.
        if let activeId = appState.activeTabId,
           let tab = appState.tab(for: activeId) {
            FileBrowserContent(
                tabId: activeId,
                tabCwd: tab.cwd,
                state: appState.fileBrowserStore.ensureState(forTab: activeId, cwd: tab.cwd)
            )
            // .id forces a fresh subtree when the tab changes, so the
            // ScrollView's scroll position resets per-tab cleanly
            // instead of carrying over stale offsets between trees.
            .id(activeId)
        } else {
            // Transient: no active tab (window between teardown and
            // next activation). Render nothing — sidebar background
            // shows through.
            Color.clear
        }
    }
}

// MARK: - Content (with bound state)

private struct FileBrowserContent: View {
    @EnvironmentObject private var appState: AppState
    @EnvironmentObject private var fontSettings: FontSettings
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    let tabId: String
    /// Tab's spawn-time CWD. Used by the breadcrumb's "Go to CWD"
    /// affordance when the current root has gone missing.
    let tabCwd: String
    @ObservedObject var state: FileBrowserState

    var body: some View {
        VStack(spacing: 0) {
            projectHeader
            breadcrumb
            Divider().opacity(0.5)
            tree
        }
    }

    // MARK: Project header

    /// Bold project-name row above the breadcrumb. Shows the owning
    /// project's name (or, for the pinned Terminals project, the
    /// tab's own title — "Terminals" alone is generic). Clicking it
    /// resets `state.rootPath` to the tab's CWD, giving the user a
    /// one-click way back to the project root after navigating into
    /// a deep subdirectory or above the project root.
    private var projectHeader: some View {
        Button(action: { state.rootPath = tabCwd }) {
            Text(appState.fileBrowserHeaderTitle(forTab: tabId))
                .font(.system(size: fontSettings.sidebarSize(13), weight: .semibold))
                .foregroundStyle(Color.niceInk(scheme, palette))
                .lineLimit(1)
                .truncationMode(.tail)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 14)
                .padding(.top, 6)
                .padding(.bottom, 2)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help("Reset to project root (\(tabCwd))")
    }

    // MARK: Breadcrumb

    private var breadcrumb: some View {
        HStack(spacing: 4) {
            SidebarSmallIconButton(
                systemImage: "chevron.up",
                help: "Go to parent folder",
                disabled: isAtFilesystemRoot
            ) {
                guard !isAtFilesystemRoot else { return }
                state.rootPath = (state.rootPath as NSString).deletingLastPathComponent
            }

            Text(displayPath)
                .font(.system(size: fontSettings.sidebarSize(11), weight: .regular))
                .foregroundStyle(Color.niceInk2(scheme, palette))
                .lineLimit(1)
                .truncationMode(.head)
                .frame(maxWidth: .infinity, alignment: .leading)
                .help(state.rootPath)

            SidebarSmallIconButton(
                systemImage: state.showHidden ? "eye" : "eye.slash",
                help: state.showHidden ? "Hide dotfiles" : "Show dotfiles"
            ) {
                state.showHidden.toggle()
            }
        }
        .padding(.horizontal, 8)
        .padding(.bottom, 6)
        .padding(.top, 2)
    }

    private var isAtFilesystemRoot: Bool {
        state.rootPath == "/" || state.rootPath.isEmpty
    }

    /// Compact path display: `~/foo/bar` if under home, last 2-3
    /// components otherwise. The tooltip carries the full path.
    private var displayPath: String {
        let home = NSHomeDirectory()
        if state.rootPath == home { return "~" }
        if state.rootPath.hasPrefix(home + "/") {
            return "~" + state.rootPath.dropFirst(home.count)
        }
        return state.rootPath
    }

    // MARK: Tree

    @ViewBuilder
    private var tree: some View {
        if FileManager.default.fileExists(atPath: state.rootPath) {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    FileTreeRow(
                        url: URL(fileURLWithPath: state.rootPath),
                        depth: 0,
                        state: state,
                        selection: state.selection,
                        tabId: tabId,
                        isRoot: true
                    )
                    // Pin SwiftUI identity to rootPath so a change of
                    // root (breadcrumb up-nav, double-click reroot,
                    // header click) throws away the row's @State —
                    // crucially the `children` cache. Without this,
                    // the same view instance is reused with a new
                    // `url` prop and the stale listing stays visible.
                    .id(state.rootPath)
                }
                .padding(.vertical, 4)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            missingFolderEmptyState
        }
    }

    private var missingFolderEmptyState: some View {
        VStack(spacing: 8) {
            Image(systemName: "folder.badge.questionmark")
                .font(.system(size: 22))
                .foregroundStyle(Color.niceInk2(scheme, palette))
            Text("Folder not found")
                .font(.system(size: fontSettings.sidebarSize(12), weight: .medium))
                .foregroundStyle(Color.niceInk(scheme, palette))
            Text(state.rootPath)
                .font(.system(size: fontSettings.sidebarSize(10)))
                .foregroundStyle(Color.niceInk2(scheme, palette))
                .lineLimit(2)
                .truncationMode(.middle)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 12)

            Button("Go to CWD") {
                state.rootPath = tabCwd
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(state.rootPath == tabCwd)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(.vertical, 24)
    }
}

// MARK: - Tree row

/// A single row in the file tree. Renders itself plus, for an
/// expanded directory, its children as nested `FileTreeRow` views.
private struct FileTreeRow: View {
    @EnvironmentObject private var appState: AppState
    @EnvironmentObject private var fontSettings: FontSettings
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    let url: URL
    let depth: Int
    @ObservedObject var state: FileBrowserState
    /// Mirror of `state.selection` so re-renders trigger when the
    /// selection set changes. Cmd-click / Shift-click handlers and
    /// the selection background both read from this.
    @ObservedObject var selection: FileBrowserSelection
    /// Tab id this row's file browser is bound to. Recorded with
    /// each file op so undo/redo can route focus back to this tab.
    let tabId: String?
    /// True for the very first row (the root). Keeps the disclosure
    /// triangle but nudges the visual treatment so the root reads as
    /// distinct from its children — and we always treat the root as
    /// expanded (it's why the user opened the browser).
    var isRoot: Bool = false

    @State private var hover: Bool = false
    @State private var children: [URL]? = nil
    /// kqueue-backed watcher started while this row is expanded.
    /// Fires a debounced reload when the directory's entries
    /// change. `@StateObject` so the instance survives view
    /// re-renders but is destroyed (and its FD released) when the
    /// row leaves the hierarchy.
    @StateObject private var watcher = DirectoryWatcher()
    /// Time of the last single-click. Used to detect a double-click
    /// without paying SwiftUI's `.onTapGesture(count:2)` disambig
    /// delay — we fire the single-click action immediately on every
    /// click and only also fire the double-click action when the
    /// second click arrives within `doubleClickWindow` of the first.
    @State private var lastTapTime: Date = .distantPast

    /// macOS's stock double-click window is ~500ms but feels long
    /// for a file tree; 280ms gives crisp feedback while still
    /// catching unhurried double-clicks.
    private static let doubleClickWindow: TimeInterval = 0.28

    private var isDirectory: Bool {
        (try? url.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) ?? false
    }

    private var path: String { url.path }

    private var isExpanded: Bool {
        state.expandedPaths.contains(path)
    }

    private var name: String {
        isRoot ? (path as NSString).lastPathComponent : url.lastPathComponent
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            row

            if isDirectory && isExpanded, let kids = children {
                ForEach(kids, id: \.self) { childURL in
                    FileTreeRow(
                        url: childURL,
                        depth: depth + 1,
                        state: state,
                        selection: selection,
                        tabId: tabId
                    )
                }
            }
        }
        .onAppear {
            if isDirectory && isExpanded {
                reloadChildren()
                startWatching()
            }
        }
        .onDisappear { watcher.stop() }
        .onChange(of: isExpanded) { _, newValue in
            if newValue {
                if children == nil { reloadChildren() }
                startWatching()
            } else {
                watcher.stop()
            }
        }
        .onChange(of: state.showHidden) { _, _ in
            // Collapsed directories must invalidate too: otherwise the
            // user toggles hidden-off, collapses, toggles back, and
            // re-expanding sees the stale (filtered) listing because
            // `.onChange(of: isExpanded)` skips reload when
            // `children != nil`. Reload now if visible; clear the
            // cache otherwise so the next expand re-reads fresh.
            guard isDirectory else { return }
            if isExpanded {
                reloadChildren()
            } else {
                children = nil
            }
        }
    }

    private var row: some View {
        // Sized to match Xcode's Project Navigator: 13pt name, 16pt
        // icon frame, 16pt indent per level, 4pt vertical padding,
        // 6pt HStack spacing. The hover background spans the full
        // padded row (with a 6pt outer margin) — same shape pattern
        // as `SidebarView.TabRow`.
        HStack(spacing: 6) {
            // Depth indent — empty horizontal slot per nesting level.
            if depth > 0 {
                Color.clear.frame(width: CGFloat(depth) * 16, height: 1)
            }

            // Disclosure triangle for directories; transparent
            // placeholder for files so names line up across rows.
            if isDirectory {
                Image(systemName: "chevron.right")
                    .font(.system(size: fontSettings.sidebarSize(10), weight: .semibold))
                    .opacity(0.7)
                    .rotationEffect(.degrees(isExpanded ? 90 : 0))
                    .frame(width: 12)
                    .animation(.easeInOut(duration: 0.1), value: isExpanded)
                    .contentShape(Rectangle())
                    .onTapGesture { toggleExpansion() }
            } else {
                Color.clear.frame(width: 12, height: 1)
            }

            Image(systemName: iconName)
                .font(.system(size: fontSettings.sidebarSize(12), weight: .regular))
                .foregroundStyle(iconColor)
                .frame(width: 16, height: 16)

            Text(name)
                .font(.system(size: fontSettings.sidebarSize(13)))
                .foregroundStyle(Color.niceInk(scheme, palette))
                .lineLimit(1)
                .truncationMode(.middle)

            Spacer(minLength: 0)
        }
        // Cut rows render at half opacity to mirror what the user
        // sees in Finder: the source dims while it's queued to
        // move, restoring once the paste completes (the adapter
        // clears the cut companion at that point).
        .opacity(isCut ? 0.45 : 1)
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(
            RoundedRectangle(cornerRadius: 4, style: .continuous)
                .fill(rowBackground)
        )
        .padding(.horizontal, 6)
        .contentShape(Rectangle())
        // Combine the row's name + icon + indent slot into one
        // addressable accessibility element. Pair with
        // `.accessibilityIdentifier` so XCUITest can locate a row
        // by its absolute path.
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("fileBrowser.row.\(path)")
        .onHover { hover = $0 }
        // Single `.onTapGesture` instead of `.onTapGesture(count: 2)`
        // + `(count: 1)` — the latter introduces a SwiftUI delay on
        // single click while it waits to disambiguate, which makes
        // expand/collapse feel laggy. We fire the single action
        // immediately and detect double-click ourselves via
        // timestamp; on a real double-click both single and double
        // actions run, which is harmless because the actions are
        // either idempotent (NSWorkspace.open) or compatible
        // (expand-then-reroot ends at the new root either way).
        .onTapGesture { handleTap() }
        .contextMenu {
            // PURE read of "which paths should the menu act on".
            // SwiftUI evaluates this closure as part of body, so it
            // must not mutate `@Published` state — the visible
            // "snap selection to right-clicked row" side effect is
            // moved into `onWillAct` below, which fires inside each
            // menu Button's action closure (i.e. *after* the menu
            // is dismissed, not during render).
            let actionPaths = selection.selectionPaths(forRightClickOn: path)
            FileBrowserContextMenu(
                clickedPath: path,
                isDirectory: isDirectory,
                isRoot: isRoot,
                actionPaths: actionPaths,
                tabId: tabId,
                onWillAct: { selection.snapIfRightClickOutside(path) },
                actions: appState
            )
        }
        .help(path)
    }

    /// Composite background: selection accent (highest priority),
    /// hover (next), or transparent. Matches the rounded-rectangle
    /// shape used for hover so the visual size doesn't jump.
    private var rowBackground: Color {
        if selection.contains(path) {
            return Color.accentColor.opacity(0.18)
        }
        if hover {
            return Color.niceInk(scheme, palette).opacity(0.06)
        }
        return Color.clear
    }

    /// True when this row's path is on the pasteboard with cut
    /// intent. Drives a dimmed rendering so the user can see what
    /// will move when they paste.
    private var isCut: Bool {
        appState.cutPaths().contains(url)
    }

    private var iconName: String {
        if isDirectory {
            return isExpanded ? "folder.fill" : "folder"
        }
        return Self.iconForFile(at: url)
    }

    private var iconColor: Color {
        if isDirectory {
            return Color.niceInk2(scheme, palette).opacity(0.9)
        }
        return Color.niceInk2(scheme, palette).opacity(0.75)
    }

    /// Single tap entry point. Fires `primaryClick()` for instant
    /// feedback on the first tap of a window; on the second tap (a
    /// double-click), runs `doubleClick()` instead so the primary
    /// action doesn't toggle expansion redundantly. Avoids SwiftUI's
    /// built-in `.onTapGesture(count:)` disambig delay, which makes
    /// expand/collapse feel laggy.
    ///
    /// Cmd-click and Shift-click are intercepted before the
    /// double-click path so they only adjust the selection (and
    /// don't expand or open).
    private func handleTap() {
        let mods = NSEvent.modifierFlags
            .intersection(KeyCombo.relevantModifierMask)
        if mods.contains(.command) {
            selection.toggle(path)
            return
        }
        if mods.contains(.shift) {
            let order = FileBrowserListing.visibleOrder(
                rootPath: state.rootPath,
                expandedPaths: state.expandedPaths,
                showHidden: state.showHidden
            )
            selection.extend(through: path, visibleOrder: order)
            return
        }
        // Plain click: replace selection with this row, then run the
        // primary/double-click action.
        let now = Date()
        let isDouble = now.timeIntervalSince(lastTapTime) < Self.doubleClickWindow
        if isDouble {
            doubleClick()
            lastTapTime = .distantPast
        } else {
            selection.replace(with: [path])
            primaryClick()
            lastTapTime = now
        }
    }

    private func primaryClick() {
        // Folders: single click toggles expansion (instant). Files:
        // single click is a no-op — we want only double-click to
        // open, mirroring Finder / Xcode navigator behavior.
        if isDirectory {
            toggleExpansion()
        }
    }

    private func doubleClick() {
        if isDirectory {
            // Re-root the browser at this folder. Combined with
            // `.id(state.rootPath)` on the top-level row, the tree
            // re-renders fresh at the new root.
            state.rootPath = path
        } else {
            NSWorkspace.shared.open(url)
        }
    }

    private func toggleExpansion() {
        state.toggleExpansion(of: path)
    }

    /// Begin watching this row's directory for entry changes. Called
    /// by `.onAppear` (when the row arrives already-expanded) and by
    /// `.onChange(of: isExpanded)` (when the user expands it). The
    /// debounced handler reloads `children` so additions / removals /
    /// renames flow into the visible tree without user action.
    private func startWatching() {
        guard isDirectory else { return }
        watcher.start(path: path) {
            reloadChildren()
        }
    }

    private func reloadChildren() {
        children = FileBrowserListing.entries(at: url, showHidden: state.showHidden)
    }

    /// Minimal extension → SF Symbol mapping. Anything not listed
    /// falls back to the generic document icon.
    private static func iconForFile(at url: URL) -> String {
        let ext = url.pathExtension.lowercased()
        switch ext {
        case "swift", "m", "mm", "h", "c", "cpp", "rs", "go", "py", "rb", "ts", "tsx", "js", "jsx":
            return "chevron.left.forwardslash.chevron.right"
        case "md", "markdown", "txt", "rst":
            return "doc.text"
        case "json", "yml", "yaml", "toml", "plist", "xml":
            return "doc.text.below.ecg"
        case "png", "jpg", "jpeg", "gif", "heic", "tiff", "bmp", "webp":
            return "photo"
        case "mp4", "mov", "m4v", "avi", "mkv":
            return "film"
        case "mp3", "wav", "aac", "m4a", "flac":
            return "music.note"
        case "pdf":
            return "doc.richtext"
        case "zip", "tar", "gz", "bz2", "xz", "7z":
            return "doc.zipper"
        case "sh", "zsh", "bash", "fish":
            return "terminal"
        default:
            return "doc"
        }
    }
}

// MARK: - Small icon button (breadcrumb-sized)

/// Compact 20pt-square icon button for the breadcrumb row. Smaller
/// than `SidebarIconButton` (24pt) so the breadcrumb doesn't dominate
/// the sidebar's vertical rhythm.
private struct SidebarSmallIconButton: View {
    @EnvironmentObject private var fontSettings: FontSettings
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    let systemImage: String
    let help: String
    var disabled: Bool = false
    let action: () -> Void

    @State private var hover = false

    var body: some View {
        Image(systemName: systemImage)
            .font(.system(size: fontSettings.sidebarSize(11), weight: .regular))
            .foregroundStyle(disabled
                ? Color.niceInk2(scheme, palette).opacity(0.4)
                : Color.niceInk2(scheme, palette))
            .frame(width: 20, height: 20)
            .background(
                RoundedRectangle(cornerRadius: 4, style: .continuous)
                    .fill(hover && !disabled
                        ? Color.niceInk(scheme, palette).opacity(0.08)
                        : Color.clear)
            )
            .contentShape(Rectangle())
            .onHover { hover = $0 }
            .onTapGesture { if !disabled { action() } }
            .help(help)
    }
}
