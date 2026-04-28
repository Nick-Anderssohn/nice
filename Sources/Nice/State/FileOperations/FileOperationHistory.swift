//
//  FileOperationHistory.swift
//  Nice
//
//  App-wide undo/redo stack for file operations driven from the
//  file-browser context menu. One instance lives on `NiceServices`,
//  shared across every window — so ⌘Z in window B can undo a trash
//  performed in window A. When the originating AppState differs from
//  the focused one, the history routes focus back to the originator
//  before applying the inverse, so the user can see the change land.
//
//  Drift handling: between push and undo, the user may move, delete,
//  or replace the affected files via Finder or the terminal. The
//  service throws on missing inputs; this layer catches and drops the
//  problem step, surfacing a transient `lastDriftMessage` for the UI
//  to flash.
//

import AppKit
import Foundation

/// Subset of `WindowRegistry` the history needs to route undo/redo
/// focus back to the originating window/tab. Pulled into its own
/// protocol so tests can stand up a fake without importing AppKit.
@MainActor
protocol FileOperationFocusRouter: AnyObject {
    /// Look up the originating AppState by its `windowSessionId`,
    /// or `nil` if its window is gone.
    func appState(forSessionId id: String) -> AppState?
    /// Bring the originating window to the front. No-op if the
    /// session id no longer maps to a live window. Fakes can record
    /// the call without importing AppKit.
    func bringToFront(sessionId id: String)
}

extension WindowRegistry: FileOperationFocusRouter {
    func bringToFront(sessionId id: String) {
        guard let window = window(forSessionId: id) else { return }
        if !window.isKeyWindow {
            window.makeKeyAndOrderFront(nil)
        }
    }
}

@MainActor
@Observable
final class FileOperationHistory {
    /// Pure FS worker the history applies / undoes through. Public
    /// so the orchestration layer (`FileExplorerOrchestrator`) shares
    /// the same instance — a `FakeTrasher` or stub `FileManager`
    /// injected here reaches copy/cut/trash too, not just undo/redo.
    let service: FileOperationsService
    @ObservationIgnored
    private weak var router: FileOperationFocusRouter?

    /// Most recent ops on top. `push` appends; `undo` removes from
    /// the end and pushes onto `redoStack`.
    private(set) var undoStack: [FileOperation] = []
    private(set) var redoStack: [FileOperation] = []

    /// One-shot transient message for the UI to surface when an op
    /// can't be undone/redone cleanly because the filesystem state
    /// has changed underneath us. Cleared by callers after they've
    /// displayed it.
    var lastDriftMessage: String?

    init(
        service: FileOperationsService = FileOperationsService(),
        registry: FileOperationFocusRouter?
    ) {
        self.service = service
        self.router = registry
    }

    // MARK: - Push

    /// Record a successful op for later undo. Pushing always clears
    /// the redo stack — once the user has performed a new op,
    /// re-applying the previously-undone ops would diverge from a
    /// linear history.
    func push(_ op: FileOperation) {
        undoStack.append(op)
        redoStack.removeAll()
    }

    // MARK: - Undo / Redo

    /// Undo the most recent op. No-op when the stack is empty.
    /// Routes focus back to the originating window/tab if it differs
    /// from the current key window; if that window is gone, applies
    /// headlessly and tells the user via `lastDriftMessage` so they
    /// know the change landed somewhere they can't see. Real drift
    /// errors (file moved/deleted by Finder between op and undo)
    /// drop the offending op rather than re-pushing to redo.
    func undo() {
        guard let op = undoStack.popLast() else { return }
        let result = followFocus(to: op.origin)
        do {
            try service.undo(op)
            redoStack.append(op)
            if result == .originGone {
                lastDriftMessage = headlessMessage(for: op, undo: true)
            }
        } catch let FileOperationError.sourceMissing(url) {
            lastDriftMessage = "Couldn't undo: '\(url.lastPathComponent)' is no longer there."
        } catch let FileOperationError.trashedItemMissing(url) {
            lastDriftMessage = "Couldn't undo: '\(url.lastPathComponent)' was emptied from Trash."
        } catch let FileOperationError.underlying(message) {
            lastDriftMessage = "Undo failed: \(message)"
        } catch {
            lastDriftMessage = "Undo failed: \(error.localizedDescription)"
        }
    }

    /// Redo the most recently undone op. No-op when the redo stack
    /// is empty. Mirrors `undo`'s focus-follow + headless-message
    /// behaviour.
    func redo() {
        guard let op = redoStack.popLast() else { return }
        let result = followFocus(to: op.origin)
        do {
            // Re-apply returns the op that was actually performed —
            // for trash this carries fresh trash URLs, for copy/move
            // it's identical to the input.
            let resultOp = try service.apply(op)
            undoStack.append(resultOp)
            if result == .originGone {
                lastDriftMessage = headlessMessage(for: op, undo: false)
            }
        } catch let FileOperationError.sourceMissing(url) {
            lastDriftMessage = "Couldn't redo: '\(url.lastPathComponent)' is no longer there."
        } catch let FileOperationError.trashedItemMissing(url) {
            lastDriftMessage = "Couldn't redo: '\(url.lastPathComponent)' was emptied from Trash."
        } catch let FileOperationError.underlying(message) {
            lastDriftMessage = "Redo failed: \(message)"
        } catch {
            lastDriftMessage = "Redo failed: \(error.localizedDescription)"
        }
    }

    // MARK: - Focus follow

    /// Result of attempting to route focus to an originating
    /// window. Used by undo/redo to decide whether to publish a
    /// headless heads-up message.
    private enum FocusResult {
        /// Focus follow ran (`bringToFront` + sidebar/tab updates).
        case routed
        /// No router configured — we're in a test or preview that
        /// doesn't care about routing. Don't surface a banner.
        case noRouter
        /// A router exists but the originating AppState is gone —
        /// the inverse will still apply, but the user should be
        /// told it landed somewhere they can't see.
        case originGone
    }

    /// Bring the originating window to the front and switch its
    /// sidebar to the file browser tab where the op happened.
    /// Returns the routing result so callers can decide whether to
    /// surface a headless-mode heads-up message.
    private func followFocus(to origin: FileOperationOrigin) -> FocusResult {
        guard let router else { return .noRouter }
        guard let appState = router.appState(forSessionId: origin.windowSessionId) else {
            return .originGone
        }
        router.bringToFront(sessionId: origin.windowSessionId)
        // Make the change visible by switching to the file browser
        // and selecting the originating tab. The tab's file browser
        // re-reads its directory via its kqueue watcher, so the
        // restored / removed entry shows up without extra plumbing.
        appState.sidebar.sidebarMode = .files
        if let tabId = origin.tabId {
            appState.tabs.selectTab(tabId)
        }
        return .routed
    }

    /// Build the message shown when an undo/redo applied without a
    /// live originating window. Says where the change landed so the
    /// user isn't surprised by a filesystem mutation they didn't
    /// directly witness.
    private func headlessMessage(for op: FileOperation, undo: Bool) -> String {
        let verb = undo ? "Undid" : "Redid"
        return "\(verb) \(op.label) — change landed in a closed window."
    }
}
