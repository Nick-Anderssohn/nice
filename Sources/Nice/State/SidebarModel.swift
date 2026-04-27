//
//  SidebarModel.swift
//  Nice
//
//  Per-window sidebar UI state. Carved out of `AppState` so view code
//  can subscribe to just the sidebar slice instead of every property
//  on the composition root.
//
//  The `@SceneStorage` bridge stays in `AppShellView`: it reads the
//  per-window stored values, passes them into `AppState.init` (which
//  threads them into this model), and writes back on changes. The
//  model itself doesn't touch SceneStorage.
//

import Foundation
import Observation

@MainActor
@Observable
final class SidebarModel {
    /// Whether the sidebar is collapsed. Seeded from the per-window
    /// `@SceneStorage` value by `AppShellView` so each window keeps
    /// its own state; the view writes back on changes.
    var sidebarCollapsed: Bool

    /// Which content the sidebar is showing (tabs vs file browser).
    /// Seeded from the per-window `@SceneStorage` value upstream so
    /// each window restores its last-used mode across relaunch.
    var sidebarMode: SidebarMode

    /// Transient: sidebar is floating over the terminal as a peek
    /// triggered by the tab-cycling shortcut while collapsed. Set by
    /// `KeyboardShortcutMonitor` after a sidebar-tab dispatch, cleared
    /// when the user releases the shortcut's modifiers. Never set while
    /// `sidebarCollapsed == false`. The view layer ORs this with its own
    /// mouse-hover pin so a hovered peek stays open after the keys lift.
    var sidebarPeeking: Bool = false

    init(initialCollapsed: Bool, initialMode: SidebarMode) {
        self.sidebarCollapsed = initialCollapsed
        self.sidebarMode = initialMode
    }

    func toggleSidebar() {
        sidebarCollapsed.toggle()
    }

    /// Flip the sidebar between projects/tabs and file-browser views.
    /// Bound to `ShortcutAction.toggleSidebarMode` (default ⌘⇧B) and
    /// the two mode icons in the sidebar header.
    func toggleSidebarMode() {
        sidebarMode = (sidebarMode == .tabs) ? .files : .tabs
    }

    /// Called by the keyboard monitor when all relevant shortcut
    /// modifiers have been released. The view's separate mouse-hover
    /// pin keeps the overlay rendered if the cursor is over it.
    func endSidebarPeek() {
        sidebarPeeking = false
    }
}
