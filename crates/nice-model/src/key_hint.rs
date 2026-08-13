//! `KeyHintModel` — the per-window "the hold-to-hint overlay is showing" flag
//! (tmux port Phase 1, D5). Pure state with no `gpui` dependency, exactly like
//! [`SidebarModel`](crate::SidebarModel)'s peek flag it mirrors: the keymap's
//! modifier observer sets/clears it, the toolbar renders from it.
//!
//! **Transient, never persisted.** It only ever describes what is on screen
//! *while the scheme's modifier pair is physically held*, so there is nothing
//! meaningful to restore — it deliberately has no serde derive and no slot in
//! any store file.
//!
//! The ~200 ms debounce that decides *when* to set it — and the pending
//! `gpui::Task` / generation counter that implements it — cannot live here (this
//! crate is gpui-free); they live on `crate::window_state::WindowState` in
//! `crates/nice`. This model is just the resulting boolean.
//!
//! Pre-splits the overlay is the window-index badges on the toolbar pills
//! (tmux `display-panes`, one digit per pill); Phase 2 reuses the same flag for
//! pane numbers, which is why the type is named for the hint rather than for the
//! badges.

/// Per-window hold-to-hint overlay state. Starts hidden; drive it with
/// [`set_visible`](KeyHintModel::set_visible).
#[derive(Debug, Default)]
pub struct KeyHintModel {
    /// Whether the hint overlay is currently rendering.
    visible: bool,
}

impl KeyHintModel {
    /// A fresh, hidden hint model.
    pub fn new() -> Self {
        KeyHintModel { visible: false }
    }

    /// Whether the hint overlay is currently rendering.
    pub fn visible(&self) -> bool {
        self.visible
    }

    /// Show (`true`) or hide (`false`) the overlay, reporting whether that
    /// actually **changed** anything. The caller uses the return value to skip a
    /// redundant `cx.notify()`: the modifier observer runs on every modifier
    /// press/release, and re-notifying on each of them would repaint the window
    /// chrome for nothing.
    pub fn set_visible(&mut self, visible: bool) -> bool {
        let changed = self.visible != visible;
        self.visible = visible;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_hidden() {
        assert!(!KeyHintModel::new().visible());
        assert!(!KeyHintModel::default().visible());
    }

    #[test]
    fn set_visible_round_trips() {
        let mut hint = KeyHintModel::new();
        hint.set_visible(true);
        assert!(hint.visible());
        hint.set_visible(false);
        assert!(!hint.visible());
    }

    #[test]
    fn set_visible_reports_only_real_changes() {
        // The notify gate: only a transition returns `true`.
        let mut hint = KeyHintModel::new();
        assert!(!hint.set_visible(false), "hidden → hidden changes nothing");
        assert!(hint.set_visible(true), "hidden → shown is a change");
        assert!(!hint.set_visible(true), "shown → shown changes nothing");
        assert!(hint.set_visible(false), "shown → hidden is a change");
    }
}
