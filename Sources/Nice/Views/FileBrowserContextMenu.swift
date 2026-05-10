//
//  FileBrowserContextMenu.swift
//  Nice
//
//  Right-click context menu for the file-browser sidebar. Defined as
//  its own view so the model layer (`AppState`, `FileBrowserState`)
//  doesn't have to know about SwiftUI menu shape, and so the menu's
//  visibility rules can be unit-tested through
//  `FileBrowserContextMenuModel`.
//
//  The view talks to the rest of the app through `FileExplorerActions`,
//  a protocol `FileExplorerOrchestrator` conforms to. Tests stand up
//  a fake conformer to verify that menu items dispatch into the
//  correct method.
//
//  Menu order, top to bottom:
//     Open
//     Open With ▸           (submenu of detected apps + Other...)
//     Open in Editor Pane ▸ (submenu of user editors + auto-detected)
//     Reveal in Finder
//     ─────
//     Copy
//     Copy Path
//     Cut
//     Paste
//     Move to Trash
//
//  Open / Open With / Open in Editor Pane are hidden on directories.
//  Cut / Copy / Trash are hidden on the file-browser root. Paste is
//  hidden when the pasteboard has no eligible file URLs.
//

import AppKit
import SwiftUI

// MARK: - Action surface

/// Protocol the menu calls into. The production conformance lives on
/// `FileExplorerOrchestrator`; tests provide their own fake conformer
/// for verifying menu dispatch.
@MainActor
protocol FileExplorerActions: AnyObject {
    func copyToPasteboard(paths: [String])
    func cutToPasteboard(paths: [String])
    func copyPathsToPasteboard(_ paths: [String])
    func pasteFromPasteboard(into target: URL, originatingTabId: String?)
    func trash(paths: [String], originatingTabId: String?)
    func open(url: URL)
    func openWith(url: URL, app: URL)
    func presentOpenWithPicker(for url: URL)
    func revealInFinder(url: URL)
    func canPaste() -> Bool
    func openWithEntries(for url: URL) -> [OpenWithEntry]
    func editorPaneEntries() -> EditorPaneEntries
    func openInEditorPane(url: URL, editorId: UUID)
    /// Notify the file-tree row at `path` that it should flip into
    /// inline-edit mode. The orchestrator publishes the request via
    /// an observable property the row watches; this loose coupling
    /// avoids the menu needing a row reference.
    func beginRename(path: String, tabId: String?)
    /// Commit a rename: move the file/folder at `oldPath` to a sibling
    /// with `newName` in the same parent. Performs the CWD-impact
    /// pre-flight (which may show an alert and short-circuit), then
    /// records an undoable `.move` op via the shared history. Drift
    /// errors (collision, source missing) surface via
    /// `history.lastDriftMessage`.
    func rename(from oldPath: String, to newName: String, originatingTabId: String?)
}

// MARK: - Pure model

/// Pure data model for which menu entries should appear. Lives
/// outside the SwiftUI view so the visibility rules are
/// unit-testable without standing up a SwiftUI environment.
struct FileBrowserContextMenuModel: Equatable {
    enum Item: Equatable {
        case open
        case openWith
        case openInEditorPane
        case revealInFinder
        case dividerOpen
        case rename
        case copy
        case copyPath
        case cut
        case paste
        case trash
    }

    let items: [Item]

    /// `canRename` is the runtime gate: false when the row is part of
    /// a multi-selection (rename is single-target only) or the row is
    /// the filesystem root `/` (which has no parent and can't be
    /// renamed). The file-browser root row (a project's CWD) is
    /// renameable — only `/` itself is the special case. The caller
    /// computes this via `FileBrowserRenameValidator.canRename` AND
    /// the selection-count check, then passes the bool here.
    static func build(
        isDirectory: Bool,
        isRoot: Bool,
        canPaste: Bool,
        canRename: Bool = true
    ) -> FileBrowserContextMenuModel {
        var out: [Item] = []
        if !isDirectory {
            out.append(.open)
            out.append(.openWith)
            out.append(.openInEditorPane)
        }
        out.append(.revealInFinder)
        out.append(.dividerOpen)
        if canRename {
            out.append(.rename)
        }
        if !isRoot {
            out.append(.copy)
        }
        out.append(.copyPath)
        if !isRoot {
            out.append(.cut)
        }
        if canPaste {
            out.append(.paste)
        }
        if !isRoot {
            out.append(.trash)
        }
        return FileBrowserContextMenuModel(items: out)
    }
}

// MARK: - View

/// The actual SwiftUI menu. Built lazily by `FileTreeRow.contextMenu`.
struct FileBrowserContextMenu: View {
    /// Path of the row the user right-clicked. Used as the paste
    /// target and as the open-with target.
    let clickedPath: String
    /// True iff `clickedPath` is a directory.
    let isDirectory: Bool
    /// True iff `clickedPath` is the root row of the browser. Cut /
    /// Copy / Trash are hidden on the root.
    let isRoot: Bool
    /// Effective set of paths to act on (single right-click → one
    /// path; right-click inside selection → all selected paths).
    let actionPaths: [String]
    /// Tab id this file browser is bound to. Recorded with each op
    /// so undo/redo can route focus back to it.
    let tabId: String?
    /// Called once, just before each menu button fires its action.
    /// Used by `FileTreeRow` to snap the selection to the clicked
    /// row when the user picked a menu item on a row outside the
    /// prior selection (Finder behaviour). Doing the snap here —
    /// inside a button's action closure — instead of inside the
    /// `.contextMenu` view builder avoids triggering an
    /// `objectWillChange` during body evaluation, which would loop
    /// the render.
    let onWillAct: () -> Void
    /// The app-side action surface. Held weakly via the unowned
    /// keyword wouldn't compile because Menu / Button capture it in
    /// closures; we use a regular reference and trust the menu's
    /// lifetime is bounded by the row (which goes away with the
    /// AppState).
    let actions: any FileExplorerActions

    var body: some View {
        // SwiftUI evaluates `contextMenu` content lazily on right-
        // click, so the pasteboard read doesn't run on every row
        // re-render — only when the menu is actually opened.
        let model = FileBrowserContextMenuModel.build(
            isDirectory: isDirectory,
            isRoot: isRoot,
            canPaste: actions.canPaste(),
            // Hide Rename when this is a multi-selection (rename is
            // single-target only) or the row is the filesystem root.
            canRename: actionPaths.count <= 1
                && FileBrowserRenameValidator.canRename(
                    URL(fileURLWithPath: clickedPath)
                )
        )
        ForEach(Array(model.items.enumerated()), id: \.offset) { _, item in
            row(for: item)
        }
    }

    @ViewBuilder
    private func row(for item: FileBrowserContextMenuModel.Item) -> some View {
        switch item {
        case .open:
            Button("Open") {
                onWillAct()
                actions.open(url: URL(fileURLWithPath: clickedPath))
            }
        case .openWith:
            openWithMenu
        case .openInEditorPane:
            editorPaneMenu
        case .revealInFinder:
            Button("Reveal in Finder") {
                onWillAct()
                actions.revealInFinder(url: URL(fileURLWithPath: clickedPath))
            }
        case .dividerOpen:
            Divider()
        case .rename:
            Button("Rename") {
                onWillAct()
                actions.beginRename(path: clickedPath, tabId: tabId)
            }
            .accessibilityIdentifier("fileBrowser.row.\(clickedPath).rename")
        case .copy:
            Button("Copy") {
                onWillAct()
                actions.copyToPasteboard(paths: actionPaths)
            }
        case .copyPath:
            Button("Copy Path") {
                onWillAct()
                actions.copyPathsToPasteboard(actionPaths)
            }
        case .cut:
            Button("Cut") {
                onWillAct()
                actions.cutToPasteboard(paths: actionPaths)
            }
        case .paste:
            Button("Paste") {
                onWillAct()
                actions.pasteFromPasteboard(
                    into: URL(fileURLWithPath: clickedPath),
                    originatingTabId: tabId
                )
            }
        case .trash:
            Button("Move to Trash") {
                onWillAct()
                actions.trash(paths: actionPaths, originatingTabId: tabId)
            }
        }
    }

    @ViewBuilder
    private var editorPaneMenu: some View {
        // Detected editors are populated off-thread at app startup;
        // if the scan hasn't returned yet we just show user-configured
        // ones (which is also what we'd show on a fresh install with
        // nothing detected). Computed inside the Menu's content
        // closure so the lookup runs only when the submenu is
        // actually presented (same rationale as `openWithMenu`).
        let url = URL(fileURLWithPath: clickedPath)
        Menu("Open in Editor Pane") {
            let entries = actions.editorPaneEntries()
            if entries.isEmpty {
                Text("No editors available")
                    .foregroundStyle(.secondary)
            } else if entries.user.isEmpty || entries.detected.isEmpty {
                // Flat list — no need for section headers when only
                // one source has entries.
                ForEach(entries.user + entries.detected) { editor in
                    editorButton(for: editor, url: url)
                }
            } else {
                Section("My editors") {
                    ForEach(entries.user) { editor in
                        editorButton(for: editor, url: url)
                    }
                }
                Section("Detected") {
                    ForEach(entries.detected) { editor in
                        editorButton(for: editor, url: url)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func editorButton(for editor: EditorCommand, url: URL) -> some View {
        Button(editor.name) {
            onWillAct()
            actions.openInEditorPane(url: url, editorId: editor.id)
        }
    }

    @ViewBuilder
    private var openWithMenu: some View {
        let url = URL(fileURLWithPath: clickedPath)
        Menu("Open With") {
            // Computed inside the Menu's content closure so the
            // Launch Services lookup only runs when the submenu is
            // actually presented, not on every parent re-render.
            // On large `/Applications` installs
            // `LSCopyApplicationURLsForURL` is a few ms.
            let entries = actions.openWithEntries(for: url)
            if entries.isEmpty {
                Text("No applications found")
                    .foregroundStyle(.secondary)
            } else {
                ForEach(entries, id: \.appURL) { entry in
                    Button {
                        actions.openWith(url: url, app: entry.appURL)
                    } label: {
                        if entry.isDefault {
                            Text("\(entry.displayName) (default)")
                        } else {
                            Text(entry.displayName)
                        }
                    }
                }
                Divider()
            }
            Button("Other…") {
                actions.presentOpenWithPicker(for: url)
            }
        }
    }
}
