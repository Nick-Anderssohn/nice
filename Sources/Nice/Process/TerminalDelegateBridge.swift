//
//  TerminalDelegateBridge.swift
//  Nice
//
//  Per-view `LocalProcessTerminalViewDelegate` that carries a `Role` tag
//  and forwards `processTerminated` into an `@MainActor` closure owned
//  by `AppState`. SwiftTerm signatures here are load-bearing —
//  `hostCurrentDirectoryUpdate` and `processTerminated` take
//  `source: TerminalView` (not `LocalProcessTerminalView`), per
//  `SwiftTerm/Sources/SwiftTerm/Mac/MacLocalTerminalView.swift`. The
//  SwiftTerm callback fires on an arbitrary queue, so we hop back to
//  MainActor via `Task { @MainActor in ... }` before touching AppState.
//

import AppKit
import SwiftTerm

final class ProcessTerminationDelegate: NSObject, LocalProcessTerminalViewDelegate {
    /// Identifies which logical slot this delegate instance is attached
    /// to. Every pane (Claude or terminal) carries a `.pane` role; the
    /// owning tab + pane id let `AppState` route exit/title callbacks.
    enum Role {
        case pane(tabId: String, paneId: String)
    }

    private let role: Role
    private let onExit: @MainActor (Role, Int32?) -> Void
    private let onTitleChange: (@MainActor (Role, String) -> Void)?
    private let onCwdChange: (@MainActor (Role, String) -> Void)?

    init(
        role: Role,
        onExit: @escaping @MainActor (Role, Int32?) -> Void,
        onTitleChange: (@MainActor (Role, String) -> Void)? = nil,
        onCwdChange: (@MainActor (Role, String) -> Void)? = nil
    ) {
        self.role = role
        self.onExit = onExit
        self.onTitleChange = onTitleChange
        self.onCwdChange = onCwdChange
    }

    func sizeChanged(source: LocalProcessTerminalView, newCols: Int, newRows: Int) {}

    func setTerminalTitle(source: LocalProcessTerminalView, title: String) {
        guard let onTitleChange else { return }
        let role = self.role
        Task { @MainActor in
            onTitleChange(role, title)
        }
    }

    /// SwiftTerm hands us the raw OSC 7 payload — typically
    /// `file://hostname/path` from the injected `_nice_emit_cwd_osc7`
    /// chpwd hook. Parse out the path component and forward to
    /// `AppState`. SwiftTerm itself does no validation, so an
    /// unparseable URL or empty/nil payload is silently dropped.
    func hostCurrentDirectoryUpdate(source: TerminalView, directory: String?) {
        guard let onCwdChange,
              let raw = directory,
              let path = Self.parseOsc7Path(raw),
              !path.isEmpty
        else { return }
        let role = self.role
        Task { @MainActor in
            onCwdChange(role, path)
        }
    }

    /// Extract the filesystem path from an OSC 7 `file://host/path`
    /// payload. Falls back to returning `raw` verbatim when it already
    /// looks absolute (no scheme, starts with `/`) — mirrors the
    /// permissive behavior of other terminals so a shell that emits a
    /// bare path still updates the cwd.
    static func parseOsc7Path(_ raw: String) -> String? {
        if raw.hasPrefix("/") { return raw }
        guard let url = URL(string: raw), url.scheme == "file" else {
            return nil
        }
        let path = url.path
        return path.isEmpty ? nil : path
    }

    func processTerminated(source: TerminalView, exitCode: Int32?) {
        let role = self.role
        let code = exitCode
        let onExit = self.onExit
        Task { @MainActor in
            onExit(role, code)
        }
    }
}
