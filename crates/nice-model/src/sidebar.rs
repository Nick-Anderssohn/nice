//! `SidebarMode` + `SidebarModel` — the per-window sidebar UI state — ported
//! from `Sources/Nice/State/SidebarModel.swift` and the `SidebarMode` enum in
//! `Sources/Nice/State/Models.swift`. Pure state with no `gpui` dependency; the
//! view layer (a later R10 slice) and the shortcut layer (R12) drive it.
//!
//! Three pieces of state: whether the sidebar is `collapsed`, which `mode` it
//! shows (tabs vs. file browser), and the transient `peeking` overlay flag. The
//! per-window `SceneStorage`/persistence bridge that seeds `collapsed` / `mode`
//! and writes them back is a view-layer concern (Swift keeps it in
//! `AppShellView`); this model just holds the values and exposes the toggles.

use serde::{Deserialize, Serialize};

/// Which content the expanded sidebar is currently showing. Window-global (one
/// mode at a time per window). Serializable for R18's per-window persistence
/// and to mirror the Swift `Codable` raw values (`Models.swift:21-26`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SidebarMode {
    /// Default — projects and tabs.
    Tabs,
    /// File-system browser rooted at the active tab's cwd.
    Files,
}

/// Per-window sidebar UI state. Construct with [`SidebarModel::new`] (seeding
/// `collapsed` / `mode` from the per-window stored values upstream); read state
/// through the getters and mutate through the toggle / peek methods.
pub struct SidebarModel {
    /// Whether the sidebar is collapsed.
    collapsed: bool,
    /// Which content the sidebar is showing (tabs vs. file browser).
    mode: SidebarMode,
    /// Transient: the sidebar is floating over the terminal as a peek. Never set
    /// while `collapsed == false` (R12 only triggers it from the collapsed
    /// tab-cycling shortcut). The view layer ORs this with its own mouse-hover
    /// pin so a hovered peek stays open after the keys lift
    /// (`SidebarModel.swift:34-37`).
    peeking: bool,
}

impl SidebarModel {
    /// Seed the model from the per-window stored collapsed/mode values
    /// (`SidebarModel.swift:39-42`). `peeking` always starts cleared.
    pub fn new(initial_collapsed: bool, initial_mode: SidebarMode) -> Self {
        SidebarModel {
            collapsed: initial_collapsed,
            mode: initial_mode,
            peeking: false,
        }
    }

    // MARK: - Query

    /// Whether the sidebar is collapsed.
    pub fn collapsed(&self) -> bool {
        self.collapsed
    }

    /// Which content the sidebar is showing.
    pub fn mode(&self) -> SidebarMode {
        self.mode
    }

    /// Whether a peek overlay is currently rendering.
    pub fn peeking(&self) -> bool {
        self.peeking
    }

    // MARK: - Toggles

    /// Flip the collapsed flag (`SidebarModel.swift:44-46`).
    pub fn toggle_sidebar(&mut self) {
        self.collapsed = !self.collapsed;
    }

    /// Flip the sidebar between projects/tabs and the file browser. Bound to
    /// the ⌘⇧B shortcut (arriving with R12) and the two mode icons in the
    /// sidebar header (`SidebarModel.swift:51-53`).
    pub fn toggle_sidebar_mode(&mut self) {
        self.mode = match self.mode {
            SidebarMode::Tabs => SidebarMode::Files,
            SidebarMode::Files => SidebarMode::Tabs,
        };
    }

    // MARK: - Peek (render / clear)

    /// Render the peek overlay — set `peeking`. R12 triggers this from a
    /// collapsed sidebar-tab cycle; its counterpart clear is
    /// [`SidebarModel::end_sidebar_peek`]. (Swift pokes the flag directly from
    /// the keyboard monitor; this method is the explicit seam R12 wires to
    /// without touching the views.)
    pub fn begin_sidebar_peek(&mut self) {
        self.peeking = true;
    }

    /// Clear the peek overlay. R12 triggers this when all relevant shortcut
    /// modifiers have been released; the view's separate mouse-hover pin keeps
    /// the overlay rendered if the cursor is over it (`SidebarModel.swift:58-60`).
    pub fn end_sidebar_peek(&mut self) {
        self.peeking = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // MARK: - toggle_sidebar

    #[test]
    fn toggle_sidebar_flips_collapsed() {
        let mut s = SidebarModel::new(false, SidebarMode::Tabs);
        s.toggle_sidebar();
        assert!(s.collapsed());
        s.toggle_sidebar();
        assert!(!s.collapsed());
    }

    #[test]
    fn toggle_sidebar_does_not_change_mode() {
        let mut s = SidebarModel::new(false, SidebarMode::Files);
        s.toggle_sidebar();
        assert_eq!(s.mode(), SidebarMode::Files);
    }

    // MARK: - toggle_sidebar_mode

    #[test]
    fn toggle_sidebar_mode_tabs_to_files() {
        let mut s = SidebarModel::new(false, SidebarMode::Tabs);
        s.toggle_sidebar_mode();
        assert_eq!(s.mode(), SidebarMode::Files);
    }

    #[test]
    fn toggle_sidebar_mode_files_to_tabs() {
        let mut s = SidebarModel::new(false, SidebarMode::Files);
        s.toggle_sidebar_mode();
        assert_eq!(s.mode(), SidebarMode::Tabs);
    }

    #[test]
    fn toggle_sidebar_mode_does_not_change_collapsed() {
        let mut s = SidebarModel::new(true, SidebarMode::Tabs);
        s.toggle_sidebar_mode();
        assert!(
            s.collapsed(),
            "Mode toggle must not change the collapsed flag."
        );
    }

    // MARK: - end_sidebar_peek

    #[test]
    fn end_sidebar_peek_clears_peek_flag() {
        let mut s = SidebarModel::new(true, SidebarMode::Tabs);
        s.begin_sidebar_peek();
        s.end_sidebar_peek();
        assert!(!s.peeking());
    }

    #[test]
    fn end_sidebar_peek_is_no_op_when_already_clear() {
        let mut s = SidebarModel::new(false, SidebarMode::Tabs);
        assert!(!s.peeking());
        s.end_sidebar_peek();
        assert!(!s.peeking());
    }

    // MARK: - peek render/clear seam

    #[test]
    fn peek_can_be_set_independently() {
        // R12 triggers the render/clear peek methods directly after a
        // sidebar-tab dispatch; the model exposes them as a plain seam (the
        // Swift keyboard monitor pokes the `sidebarPeeking` var).
        let mut s = SidebarModel::new(true, SidebarMode::Tabs);
        s.begin_sidebar_peek();
        assert!(s.peeking());
        s.end_sidebar_peek();
        assert!(!s.peeking());
    }
}
