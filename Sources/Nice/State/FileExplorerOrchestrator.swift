//
//  FileExplorerOrchestrator.swift
//  Nice
//
//  Per-window orchestration glue between the file-browser context
//  menu and the rest of the app. The menu calls the protocol methods
//  declared in `FileBrowserContextMenu`; this is the production
//  conformance.
//
//  Each method records a `FileOperationOrigin` from the owning
//  window's `windowSessionId` and the active tab id so undo/redo can
//  route focus back to where the change happened. Errors are surfaced
//  via the shared history's `lastDriftMessage` for the UI to flash.
//
//  Carved out of `AppState` so the file-explorer surface stops
//  inflating the composition root. AppState now holds this as one
//  of its sub-models alongside TabModel/SessionsModel/etc.
//

import AppKit
import Foundation

@MainActor
@Observable
final class FileExplorerOrchestrator: FileExplorerActions {

    /// Read for `activeTabId` / `tab(for:)` / `firstAvailableTabId`.
    /// Weak — owned by AppState alongside us.
    @ObservationIgnored
    weak var tabs: TabModel?

    /// Reads `addPane` to spawn an editor pane on the resolved tab.
    @ObservationIgnored
    weak var sessions: SessionsModel?

    /// Reads `windowSessionId` for `FileOperationOrigin` so undo/redo
    /// routes focus back to this window.
    @ObservationIgnored
    weak var windowSession: WindowSession?

    /// File-browser context-menu services. `nil` for previews/tests.
    let fileExplorer: FileExplorerServices?

    /// User preferences — `openInEditorPane` reads editor mappings.
    let tweaks: Tweaks?

    /// Auto-detected terminal editors for `editorPaneEntries`.
    let editorDetector: EditorDetector?

    /// Confirmation hook for the CWD-invalidation pre-flight on
    /// rename. Production wires this to `NSAlertRenameConfirmer`
    /// (defined below); unit tests inject a stub returning a canned
    /// answer so `rename(from:to:)` is testable without `runModal`.
    /// `nil` is treated as "no confirmer available" → the rename
    /// proceeds without prompting (acceptable for previews/tests
    /// that don't care).
    @ObservationIgnored
    var renameConfirmer: RenameImpactConfirmer?

    /// Notification channel for the right-click "Rename" menu item.
    /// The menu calls `beginRename(path:tabId:)`, which sets this
    /// property; each `FileTreeRow` observes it via SwiftUI and
    /// flips into edit mode iff its own path matches, then clears
    /// the property. Loose coupling — the menu needs no row
    /// reference, and only one row can possibly match.
    var pendingRenamePath: String?

    init(
        fileExplorer: FileExplorerServices?,
        tweaks: Tweaks?,
        editorDetector: EditorDetector?,
        renameConfirmer: RenameImpactConfirmer? = nil
    ) {
        self.fileExplorer = fileExplorer
        self.tweaks = tweaks
        self.editorDetector = editorDetector
        self.renameConfirmer = renameConfirmer
    }

    // MARK: - Pasteboard write

    func copyToPasteboard(paths: [String]) {
        guard let fileExplorer else { return }
        let urls = paths.map { URL(fileURLWithPath: $0) }
        fileExplorer.pasteboard.write(urls: urls, intent: .copy)
    }

    func cutToPasteboard(paths: [String]) {
        guard let fileExplorer else { return }
        let urls = paths.map { URL(fileURLWithPath: $0) }
        fileExplorer.pasteboard.write(urls: urls, intent: .cut)
    }

    func copyPathsToPasteboard(_ paths: [String]) {
        guard let fileExplorer else { return }
        // "Copy Path" writes plain-text path(s), not file URLs.
        // Multiple paths are joined with newlines, matching what
        // Finder's "Copy as Pathname" produces.
        fileExplorer.pasteboard.writeText(paths.joined(separator: "\n"))
    }

    // MARK: - Paste

    /// Paste pasteboard contents into `target`. If `target` is a
    /// directory, items land inside it. If it's a file, items land
    /// in the file's parent directory. Collisions auto-rename per
    /// `FileOperationsService.nextAvailableName`.
    func pasteFromPasteboard(into target: URL, originatingTabId: String?) {
        guard let fileExplorer else { return }
        guard let read = fileExplorer.pasteboard.read() else { return }

        let dest = Self.resolvePasteDestination(target: target)
        let origin = FileOperationOrigin(
            windowSessionId: windowSession?.windowSessionId ?? "",
            tabId: originatingTabId ?? tabs?.activeTabId
        )

        do {
            let op: FileOperation
            switch read.intent {
            case .copy:
                op = try fileExplorer.service.copy(items: read.urls, into: dest, origin: origin)
            case .cut:
                op = try fileExplorer.service.move(items: read.urls, into: dest, origin: origin)
                fileExplorer.pasteboard.clearCutIntent()
            }
            fileExplorer.history.push(op)
        } catch let FileOperationError.sourceMissing(url) {
            fileExplorer.history.lastDriftMessage =
                "Couldn't paste: '\(url.lastPathComponent)' is no longer there."
        } catch let FileOperationError.underlying(message) {
            fileExplorer.history.lastDriftMessage = "Paste failed: \(message)"
        } catch {
            fileExplorer.history.lastDriftMessage =
                "Paste failed: \(error.localizedDescription)"
        }
    }

    /// Resolve where a paste should land for a given context-menu
    /// target row. Right-clicking a directory pastes inside it;
    /// right-clicking a file pastes into its parent. Pure — kept
    /// `static` so it's testable without an instance.
    static func resolvePasteDestination(target: URL) -> URL {
        var isDir: ObjCBool = false
        let exists = FileManager.default.fileExists(
            atPath: target.path,
            isDirectory: &isDir
        )
        if exists && isDir.boolValue {
            return target
        }
        return target.deletingLastPathComponent()
    }

    // MARK: - Drag-and-drop move / copy

    /// Move or copy `urls` into `dest`, recording an undoable
    /// `FileOperation` on the shared history. Drag-and-drop calls
    /// this directly rather than going through the pasteboard:
    /// there's no pasteboard intent to read, the destination is
    /// already known, and the operation is one-shot.
    ///
    /// Errors are surfaced via the same `lastDriftMessage` channel
    /// the cut-and-paste flow uses, so the UI's drift banner stays
    /// the single user-facing surface for filesystem-op failures.
    func moveOrCopy(
        urls: [URL],
        into dest: URL,
        operation: FileDragOperation,
        originatingTabId: String?
    ) {
        guard let fileExplorer else { return }
        guard !urls.isEmpty else { return }
        let origin = FileOperationOrigin(
            windowSessionId: windowSession?.windowSessionId ?? "",
            tabId: originatingTabId ?? tabs?.activeTabId
        )

        do {
            let op: FileOperation
            switch operation {
            case .copy:
                op = try fileExplorer.service.copy(items: urls, into: dest, origin: origin)
            case .move:
                op = try fileExplorer.service.move(items: urls, into: dest, origin: origin)
            }
            fileExplorer.history.push(op)
        } catch let FileOperationError.sourceMissing(url) {
            let verb = (operation == .copy) ? "copy" : "move"
            fileExplorer.history.lastDriftMessage =
                "Couldn't \(verb): '\(url.lastPathComponent)' is no longer there."
        } catch let FileOperationError.underlying(message) {
            let verb = (operation == .copy) ? "Copy" : "Move"
            fileExplorer.history.lastDriftMessage = "\(verb) failed: \(message)"
        } catch {
            let verb = (operation == .copy) ? "Copy" : "Move"
            fileExplorer.history.lastDriftMessage =
                "\(verb) failed: \(error.localizedDescription)"
        }
    }

    // MARK: - Trash

    func trash(paths: [String], originatingTabId: String?) {
        guard let fileExplorer else { return }
        let urls = paths.map { URL(fileURLWithPath: $0) }
        let origin = FileOperationOrigin(
            windowSessionId: windowSession?.windowSessionId ?? "",
            tabId: originatingTabId ?? tabs?.activeTabId
        )

        do {
            let op = try fileExplorer.service.trash(items: urls, origin: origin)
            fileExplorer.history.push(op)
        } catch let FileOperationError.sourceMissing(url) {
            fileExplorer.history.lastDriftMessage =
                "Couldn't trash: '\(url.lastPathComponent)' is no longer there."
        } catch let FileOperationError.underlying(message) {
            fileExplorer.history.lastDriftMessage = "Trash failed: \(message)"
        } catch {
            fileExplorer.history.lastDriftMessage =
                "Trash failed: \(error.localizedDescription)"
        }
    }

    // MARK: - Open

    func open(url: URL) {
        NSWorkspace.shared.open(url)
    }

    func openWith(url: URL, app: URL) {
        let config = NSWorkspace.OpenConfiguration()
        NSWorkspace.shared.open([url], withApplicationAt: app, configuration: config)
    }

    /// Present a system app picker rooted at `/Applications` so the
    /// user can pick an arbitrary app to open `url` with. Lives here
    /// rather than the menu view so the view layer doesn't drive UI
    /// flow control via `NSOpenPanel.runModal`.
    func presentOpenWithPicker(for url: URL) {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.canChooseFiles = true
        panel.directoryURL = URL(fileURLWithPath: "/Applications")
        panel.allowedContentTypes = [.application]
        panel.prompt = "Open"
        panel.message = "Choose an application to open '\(url.lastPathComponent)'"
        if panel.runModal() == .OK, let app = panel.url {
            openWith(url: url, app: app)
        }
    }

    // MARK: - Reveal in Finder

    func revealInFinder(url: URL) {
        NSWorkspace.shared.activateFileViewerSelecting([url])
    }

    // MARK: - Pasteboard query

    func canPaste() -> Bool {
        fileExplorer?.pasteboard.read() != nil
    }

    /// Snapshot of paths currently flagged as "cut" on the
    /// pasteboard. Views read this to ghost cut rows.
    func cutPaths() -> Set<URL> {
        fileExplorer?.pasteboard.cutPaths ?? []
    }

    // MARK: - Open With provider

    func openWithEntries(for url: URL) -> [OpenWithEntry] {
        fileExplorer?.openWithProvider.entries(for: url) ?? []
    }

    // MARK: - Editor pane

    /// Single entry point for File Explorer double-clicks on a file.
    /// Routes to the editor-pane path when the extension is mapped,
    /// otherwise falls through to the OS default app handler. Lives
    /// here (not in the view) so the routing rule is pinned in one
    /// place — the right-click submenu and the double-click default
    /// have to agree on what an editor mapping means, and the view
    /// layer shouldn't be the one enforcing that.
    func openFromDoubleClick(url: URL) {
        if let editor = tweaks?.editor(forExtension: url.pathExtension) {
            openInEditorPane(url: url, editorId: editor.id)
        } else {
            NSWorkspace.shared.open(url)
        }
    }

    /// Returns the user-configured + auto-detected editor lists for
    /// the "Open in Editor Pane" submenu, deduplicated by `command`
    /// so a manually-added `vim` doesn't appear twice when `vim` is
    /// also auto-detected.
    func editorPaneEntries() -> EditorPaneEntries {
        Self.mergeEditorPaneEntries(
            user: tweaks?.editorCommands ?? [],
            detected: editorDetector?.detected ?? []
        )
    }

    /// Pure dedup-by-command merge. User-configured editors win on
    /// collision so any custom args the user set on their entry take
    /// precedence over the detected default. Lifted out for direct
    /// unit testing.
    static func mergeEditorPaneEntries(
        user: [EditorCommand],
        detected: [EditorCommand]
    ) -> EditorPaneEntries {
        let userCommands = Set(user.map(\.command))
        let filtered = detected.filter { !userCommands.contains($0.command) }
        return EditorPaneEntries(user: user, detected: filtered)
    }

    /// Spawn an editor pane for `url` using the editor identified by
    /// `editorId`. Looks the editor up first in user config, then in
    /// the detected list. Pane lands in the currently active tab,
    /// falling back to the first available tab if none is active.
    /// No-op when no editor or no tab is available.
    func openInEditorPane(url: URL, editorId: UUID) {
        let editor = tweaks?.editor(for: editorId)
            ?? editorDetector?.detected.first { $0.id == editorId }
        guard let editor else { return }
        guard let tabs, let sessions else { return }

        guard let tabId = Self.resolveTargetTab(
            activeTabId: tabs.activeTabId,
            hasTab: { tabs.tab(for: $0) != nil },
            firstAvailable: { tabs.firstAvailableTabId() }
        ) else { return }

        let spec = Self.editorPaneSpec(editor: editor, url: url)
        sessions.addPane(
            tabId: tabId,
            kind: .terminal,
            cwd: spec.cwd,
            title: spec.title,
            command: spec.command
        )
        if tabs.activeTabId != tabId {
            tabs.activeTabId = tabId
        }
    }

    /// Pure tab resolver — the active tab when present, otherwise the
    /// first available tab in sidebar order. Lifted out for direct
    /// unit testing without standing up an AppState. Returns nil when
    /// no tab exists anywhere (theoretical — the Terminals project's
    /// Main tab is the boot invariant).
    static func resolveTargetTab(
        activeTabId: String?,
        hasTab: (String) -> Bool,
        firstAvailable: () -> String?
    ) -> String? {
        if let active = activeTabId, hasTab(active) {
            return active
        }
        return firstAvailable()
    }

    /// Pure projection of (editor, url) → spawn arguments. Pulled out
    /// for direct unit testing — verifying shell-quoting on weird
    /// paths and the `cwd = parent directory` decision shouldn't need
    /// to construct an AppState.
    static func editorPaneSpec(
        editor: EditorCommand,
        url: URL
    ) -> EditorPaneSpec {
        let parent = url.deletingLastPathComponent().path
        let quoted = shellSingleQuote(url.path)
        // Editor.command is parsed by zsh (so `nvim -p` works); only
        // the file path is quoted to survive spaces/special characters.
        let command = "\(editor.command) \(quoted)"
        let title = "\(editor.name) \(url.lastPathComponent)"
        return EditorPaneSpec(cwd: parent, title: title, command: command)
    }

    // MARK: - Undo / Redo (also wired through KeyboardShortcutMonitor)

    func undoFileOperation() {
        fileExplorer?.history.undo()
    }

    func redoFileOperation() {
        fileExplorer?.history.redo()
    }

    // MARK: - Rename

    /// Test seam — incremented every time `focusActiveTerminal()` is
    /// called. Exposed so unit tests can verify the post-rename focus
    /// hand-off fires without having to stand up an `NSWindow` to
    /// observe `makeFirstResponder`. Tests reset to 0 as needed; in
    /// production it's a harmless counter that never reaches anything
    /// observable.
    @ObservationIgnored
    private(set) var focusActiveTerminalCallCount: Int = 0

    /// Hand AppKit first-responder status back to the active pane's
    /// terminal after the rename field is torn down. SwiftUI doesn't
    /// restore focus to an embedded `NSView` when a TextField goes
    /// away, so without this the user can't type into the terminal
    /// (or click it back into focus reliably) after a rename. Same
    /// fix the sidebar tab and pane pill renames use. Safe no-op if
    /// `sessions` is nil (previews, tests).
    func focusActiveTerminal() {
        focusActiveTerminalCallCount += 1
        sessions?.focusActiveTerminal()
    }

    func beginRename(path: String, tabId: String?) {
        // Defense-in-depth: the menu-side gate already hides
        // "Rename" for `/`. Re-checking here means a rogue caller
        // (or a future Return-key handler) can't bypass the gate.
        guard FileBrowserRenameValidator.canRename(URL(fileURLWithPath: path)) else {
            return
        }
        pendingRenamePath = path
    }

    func rename(from oldPath: String, to newName: String, originatingTabId: String?) {
        // The row tears down the rename field before calling us, so
        // the responder chain has lost its first responder. SwiftUI
        // doesn't restore focus to embedded NSViews — same fix the
        // pane pill rename uses. Defer so EVERY exit path (success,
        // CWD-impact cancel, collision drift, format-guard early
        // return) restores focus; impossible to forget when adding a
        // new branch.
        defer { focusActiveTerminal() }
        guard let fileExplorer else { return }
        let oldURL = URL(fileURLWithPath: oldPath)
        let trimmed = newName.trimmingCharacters(in: .whitespacesAndNewlines)
        // The row's `commitRename` should have validated already, but
        // we mirror the format checks here so a rogue caller can't
        // smuggle in an empty or slash-bearing name.
        guard !trimmed.isEmpty, !trimmed.contains("/"), !trimmed.contains(":") else {
            return
        }
        let newURL = oldURL.deletingLastPathComponent().appendingPathComponent(trimmed)

        // CWD-invalidation pre-flight. Only directories can host a
        // shell, but we run the scan unconditionally — a file rename
        // is never going to match, so the cost is one wasted walk.
        if let registry = fileExplorer.registry {
            let snapshot = FileBrowserCWDImpactCheck.snapshot(from: registry)
            let affected = FileBrowserCWDImpactCheck.affectedBy(
                rename: oldPath, snapshot: snapshot
            )
            if !affected.isEmpty {
                let proceed = renameConfirmer?.confirmRenameDespiteCWDImpact(
                    affected: affected,
                    oldPath: oldPath
                ) ?? true  // No confirmer wired — proceed.
                guard proceed else { return }
            }
        }

        let origin = FileOperationOrigin(
            windowSessionId: windowSession?.windowSessionId ?? "",
            tabId: originatingTabId ?? tabs?.activeTabId
        )
        let pair = FileOperationItem(source: oldURL, destination: newURL)

        do {
            // Use `apply(.move:)` rather than `move(items:into:)` so
            // the Finder-style auto-suffix in `nextAvailableName` is
            // bypassed — for rename we want collisions to surface as
            // a drift error, not silently land at "foo copy.txt".
            let op = try fileExplorer.service.apply(
                .move(items: [pair], origin: origin)
            )
            fileExplorer.history.push(op)
        } catch let FileOperationError.sourceMissing(url) {
            fileExplorer.history.lastDriftMessage =
                "Couldn't rename: '\(url.lastPathComponent)' is no longer there."
        } catch let FileOperationError.underlying(message) {
            fileExplorer.history.lastDriftMessage =
                Self.renameDriftMessage(forUnderlying: message, newName: trimmed)
        } catch {
            fileExplorer.history.lastDriftMessage =
                "Rename failed: \(error.localizedDescription)"
        }
    }

    /// Pattern-match the Foundation collision message so the user
    /// sees a stable string regardless of macOS version. Falls back
    /// to the raw underlying message for anything unrecognized.
    /// Pure — exposed so tests can pin the exact phrasing without
    /// running a real `apply(.move)` against a colliding path.
    static func renameDriftMessage(forUnderlying message: String, newName: String) -> String {
        // `NSCocoaErrorDomain` `NSFileWriteFileExistsError` (516) is
        // surfaced as "...File ... couldn't be written because a
        // file with the same name already exists." across recent
        // macOS versions. Match on the stable substring.
        if message.contains("already exists") {
            return "Couldn't rename: '\(newName)' already exists."
        }
        return "Rename failed: \(message)"
    }
}

// MARK: - Rename impact confirmer

/// Decision protocol consulted before applying a rename that would
/// invalidate one or more open terminal CWDs. Production conformance
/// shows an `NSAlert.runModal`; tests inject a stub.
@MainActor
protocol RenameImpactConfirmer: AnyObject {
    /// Return `true` if the user wants to proceed despite the
    /// invalidation. Implementations are expected to be synchronous
    /// (modal); the orchestrator stalls the rename on the response.
    func confirmRenameDespiteCWDImpact(
        affected: [PaneCWDRef],
        oldPath: String
    ) -> Bool
}

/// Production confirmer. Lives next to the orchestrator because
/// it's the only caller. The view layer wires one of these onto
/// `FileExplorerOrchestrator.renameConfirmer` after both AppKit and
/// the AppState are fully constructed.
@MainActor
final class NSAlertRenameConfirmer: RenameImpactConfirmer {
    func confirmRenameDespiteCWDImpact(
        affected: [PaneCWDRef],
        oldPath: String
    ) -> Bool {
        let count = affected.count
        let alert = NSAlert()
        alert.messageText = "Rename will affect \(count) open terminal\(count == 1 ? "" : "s")"
        let folder = (oldPath as NSString).lastPathComponent
        alert.informativeText = "Renaming '\(folder)' will break the working directory of \(count) open terminal\(count == 1 ? "" : "s"). The terminal\(count == 1 ? "" : "s") will keep running but `pwd` will report a path that no longer exists. Rename anyway?"
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Cancel")
        let renameButton = alert.addButton(withTitle: "Rename Anyway")
        renameButton.hasDestructiveAction = true
        return alert.runModal() == .alertSecondButtonReturn
    }
}

/// Partitioned view onto the editor list the context menu renders.
/// Detected editors that share a `command` with a user-configured one
/// have already been filtered out so each command appears at most
/// once across both arrays.
struct EditorPaneEntries: Hashable, Sendable {
    let user: [EditorCommand]
    let detected: [EditorCommand]

    static let empty = EditorPaneEntries(user: [], detected: [])

    var isEmpty: Bool { user.isEmpty && detected.isEmpty }
}

/// Pure projection of (editor, file URL) into the spawn arguments
/// `FileExplorerOrchestrator.openInEditorPane` hands to `addPane`.
/// Lives as a named type so the test surface is `spec.command`
/// against a struct rather than a positional tuple, and so adding a
/// fourth field later isn't source-breaking.
struct EditorPaneSpec: Hashable, Sendable {
    let cwd: String
    let title: String
    let command: String
}
