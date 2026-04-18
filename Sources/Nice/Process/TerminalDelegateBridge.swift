//
//  TerminalDelegateBridge.swift
//  Nice
//
//  Phase 4: shared no-op `LocalProcessTerminalViewDelegate` used by both
//  `TabPtySession` and `MainTerminalSession`. The SwiftTerm protocol
//  signatures here are load-bearing — `hostCurrentDirectoryUpdate` and
//  `processTerminated` take `source: TerminalView` (not
//  `LocalProcessTerminalView`), per
//  `SwiftTerm/Sources/SwiftTerm/Mac/MacLocalTerminalView.swift`.
//

import AppKit
import SwiftTerm

final class TerminalDelegateBridge: NSObject, LocalProcessTerminalViewDelegate {
    func sizeChanged(source: LocalProcessTerminalView, newCols: Int, newRows: Int) {}
    func setTerminalTitle(source: LocalProcessTerminalView, title: String) {}
    func hostCurrentDirectoryUpdate(source: TerminalView, directory: String?) {}
    func processTerminated(source: TerminalView, exitCode: Int32?) {}
}
