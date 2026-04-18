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
    /// to. `AppState` fans the exit callback out to the right handler
    /// based on this tag.
    enum Role {
        case main
        case claude(tabId: String)
        case companion(tabId: String, companionId: String)
    }

    private let role: Role
    private let onExit: @MainActor (Role, Int32?) -> Void

    init(role: Role, onExit: @escaping @MainActor (Role, Int32?) -> Void) {
        self.role = role
        self.onExit = onExit
    }

    func sizeChanged(source: LocalProcessTerminalView, newCols: Int, newRows: Int) {}
    func setTerminalTitle(source: LocalProcessTerminalView, title: String) {}
    func hostCurrentDirectoryUpdate(source: TerminalView, directory: String?) {}

    func processTerminated(source: TerminalView, exitCode: Int32?) {
        let role = self.role
        let code = exitCode
        let onExit = self.onExit
        Task { @MainActor in
            onExit(role, code)
        }
    }
}
