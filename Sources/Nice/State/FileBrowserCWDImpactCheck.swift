//
//  FileBrowserCWDImpactCheck.swift
//  Nice
//
//  Pure validator that decides whether renaming a file or folder
//  would invalidate any open terminal pane's working directory. Any
//  pane whose live `cwd` (or its tab's anchor `cwd`) equals the
//  renamed path or is a descendant of it is "affected" — after the
//  move on disk that pane is sitting in a path that no longer exists.
//
//  Snapshot is built from `WindowRegistry.allAppStates` so the scan
//  spans every open window, both Claude and terminal panes. The
//  actual `affectedBy(...)` algorithm is purely string-prefix and
//  unit-testable without standing up windows.
//
//  We include both per-pane and per-tab CWDs in the snapshot. Per-
//  pane (`Pane.cwd`) is the live OSC-7 tracked directory of the
//  shell; the tab-level `Tab.cwd` is the anchor used for
//  `claude --resume` and as a fallback when a pane hasn't emitted an
//  OSC 7 yet. Both are user-visible and would break if invalidated.
//

import Foundation

/// One CWD reference captured by the snapshot. Either a live pane or
/// a tab anchor. Pane references carry the `kind`; tab anchors use
/// `.terminal` as a sentinel — the alert message doesn't distinguish
/// kinds, it just counts.
struct PaneCWDRef: Equatable, Sendable {
    let windowSessionId: String
    let tabId: String
    /// Empty string for tab-anchor entries (`Tab.cwd` rather than a
    /// pane). Real pane ids are non-empty.
    let paneId: String
    let kind: PaneKind
    /// Absolute path. Trailing slash is normalized off in the snapshot
    /// builder so prefix matching is straightforward.
    let cwd: String
}

/// Flat list of every CWD reference across every window. Built once
/// at the start of a rename attempt so the validator runs against a
/// consistent view of the world.
struct PaneCWDSnapshot: Equatable, Sendable {
    let entries: [PaneCWDRef]
}

enum FileBrowserCWDImpactCheck {

    /// Return every snapshot entry whose `cwd` would be invalidated
    /// by renaming `oldPath`. Match rule: `cwd == oldPath` (the user
    /// is renaming the exact directory the shell is in), OR
    /// `cwd.hasPrefix(oldPath + "/")` (the user is renaming an
    /// ancestor of the shell's directory).
    ///
    /// `oldPath` is normalized to drop a trailing slash so callers
    /// can pass either form. `oldPath == "/"` is excluded by the
    /// `canRename` gate at the trigger layer; we handle it here too
    /// (every CWD would match) by returning an empty list — there's
    /// no useful rename to warn about.
    static func affectedBy(
        rename oldPath: String,
        snapshot: PaneCWDSnapshot
    ) -> [PaneCWDRef] {
        let normalized = normalizePath(oldPath)
        // Filesystem root would `hasPrefix("/" + "/")` against
        // every absolute path (vacuously true after the prefix
        // forms "//"). It's also nonsensical to warn about. Bail.
        guard normalized != "/" else { return [] }
        let prefix = normalized + "/"
        return snapshot.entries.filter { entry in
            let cwd = normalizePath(entry.cwd)
            return cwd == normalized || cwd.hasPrefix(prefix)
        }
    }

    /// Strip a single trailing `/` from `path` (other than the root
    /// `/`). The snapshot builder runs every `cwd` through this so
    /// equality and prefix tests in `affectedBy` see canonical forms.
    static func normalizePath(_ path: String) -> String {
        guard path.count > 1, path.hasSuffix("/") else { return path }
        return String(path.dropLast())
    }
}

#if canImport(AppKit)
import AppKit

extension FileBrowserCWDImpactCheck {
    /// Walk the registry and emit a snapshot. `@MainActor` because
    /// `WindowRegistry` and `TabModel` reads are main-actor isolated.
    /// Skips `!pane.isAlive`. Includes a synthetic tab-anchor entry
    /// for every tab so the warning fires for tabs whose panes haven't
    /// emitted an OSC 7 yet (their effective CWD is `Tab.cwd`).
    @MainActor
    static func snapshot(from registry: WindowRegistry) -> PaneCWDSnapshot {
        var entries: [PaneCWDRef] = []
        for appState in registry.allAppStates {
            let windowSessionId = appState.windowSession.windowSessionId
            for project in appState.tabs.projects {
                for tab in project.tabs {
                    // Tab anchor — represented with an empty paneId
                    // and `.terminal` sentinel kind.
                    entries.append(PaneCWDRef(
                        windowSessionId: windowSessionId,
                        tabId: tab.id,
                        paneId: "",
                        kind: .terminal,
                        cwd: normalizePath(tab.cwd)
                    ))
                    for pane in tab.panes where pane.isAlive {
                        guard let cwd = pane.cwd, !cwd.isEmpty else { continue }
                        entries.append(PaneCWDRef(
                            windowSessionId: windowSessionId,
                            tabId: tab.id,
                            paneId: pane.id,
                            kind: pane.kind,
                            cwd: normalizePath(cwd)
                        ))
                    }
                }
            }
        }
        return PaneCWDSnapshot(entries: entries)
    }
}
#endif
