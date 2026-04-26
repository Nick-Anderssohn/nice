//
//  FileBrowserSelection.swift
//  Nice
//
//  Per-file-browser multi-row selection model. One instance lives on
//  each `FileBrowserState`. Captures three pieces of state:
//
//    • `selectedPaths` — current selection. The right-click menu
//      acts on these when the right-clicked path is in the set.
//    • `lastClickedPath` — anchor for Shift-range selection.
//    • selection-vs-clicked policy in `selectionPaths(for:)`, so the
//      view layer doesn't have to repeat the "right-click outside
//      selection replaces it" rule.
//
//  No SwiftUI / view dependency — the model is fully unit-testable.
//

import Foundation

@MainActor
final class FileBrowserSelection: ObservableObject {
    /// Absolute paths currently selected. Empty by default.
    @Published private(set) var selectedPaths: Set<String> = []

    /// Anchor for Shift-range. Set on plain click and Cmd-click;
    /// not set by Shift-click (Finder keeps the original anchor
    /// across multiple range extensions).
    @Published private(set) var lastClickedPath: String?

    init() {}

    // MARK: - Mutation

    /// Replace the selection with exactly `paths`. Called on plain
    /// click (single path) and right-click outside selection.
    func replace(with paths: [String], anchor: String? = nil) {
        selectedPaths = Set(paths)
        if let anchor {
            lastClickedPath = anchor
        } else {
            lastClickedPath = paths.last
        }
    }

    /// Toggle `path` in/out of the selection (Cmd-click).
    func toggle(_ path: String) {
        if selectedPaths.contains(path) {
            selectedPaths.remove(path)
        } else {
            selectedPaths.insert(path)
        }
        lastClickedPath = path
    }

    /// Extend the selection to span from `lastClickedPath` to
    /// `path`, inclusive, using the given visible row order. If
    /// there is no anchor yet, behaves like `replace(with: [path])`.
    /// The anchor is *not* updated — Finder keeps the original
    /// anchor across multiple range extensions.
    func extend(through path: String, visibleOrder: [String]) {
        guard let anchor = lastClickedPath else {
            selectedPaths = [path]
            lastClickedPath = path
            return
        }
        guard let aIdx = visibleOrder.firstIndex(of: anchor),
              let bIdx = visibleOrder.firstIndex(of: path) else {
            // Anchor or target isn't in the visible order; fall back
            // to a plain replace so the selection still moves.
            selectedPaths = [path]
            return
        }
        let lo = min(aIdx, bIdx)
        let hi = max(aIdx, bIdx)
        selectedPaths = Set(visibleOrder[lo...hi])
        // Don't move `lastClickedPath` — keep the anchor.
    }

    /// Clear the selection entirely.
    func clear() {
        selectedPaths = []
        lastClickedPath = nil
    }

    // MARK: - Query

    func contains(_ path: String) -> Bool {
        selectedPaths.contains(path)
    }

    /// Resolve the effective set of paths to act on for a right-
    /// click on `clickedPath`. If the clicked path is in the
    /// selection, return the whole selection. Otherwise, replace
    /// the selection with just `clickedPath` (a side effect — the
    /// selection visibly snaps to the clicked row, matching
    /// Finder), and return that single path.
    func selectionPaths(forRightClickOn clickedPath: String) -> [String] {
        if selectedPaths.contains(clickedPath) {
            return Array(selectedPaths)
        }
        replace(with: [clickedPath])
        return [clickedPath]
    }
}
