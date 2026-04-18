//
//  TerminalHost.swift
//  Nice
//
//  Phase 4: SwiftUI wrapper around a stable `LocalProcessTerminalView`
//  instance owned by the containing session. `updateNSView` is a no-op
//  by design — the session object (and therefore the view) outlives any
//  individual SwiftUI redraw, keeping the pty and its scrollback alive
//  across tab switches.
//

import SwiftTerm
import SwiftUI

struct TerminalHost: NSViewRepresentable {
    let view: LocalProcessTerminalView

    func makeNSView(context: Context) -> LocalProcessTerminalView { view }
    func updateNSView(_ nsView: LocalProcessTerminalView, context: Context) {}
}
