//! `SharedSidebarFontSettings` — the app-level sidebar-font state (R23 Binding
//! decision D3, What-to-build item 4).
//!
//! **Boundary (TRANCHE-2-NOTES §4, binding):** sidebar font size is NOT a terminal
//! concept, so it must NOT live in `nice-term-view::FontSettings`. It lives here,
//! in an app-level gpui entity in `crates/nice`, mirroring the `SharedFontSettings`
//! Global idiom (`keymap.rs`). It holds the sidebar base point size + the ported
//! [`sidebar_size`] proportional-scale helper the sidebar chrome reads.
//!
//! ## Independent of the terminal size
//! The sidebar size changes only through its own Font-pane stepper, the Font
//! pane's "Reset to defaults", and the keyboard zoom (⌘=/⌘−/⌘0), which steps the
//! terminal and sidebar sizes by 1pt each (`keymap.rs` `zoom_shared_font`). There
//! is no ratio coupling: a terminal-size change never moves the sidebar.

use gpui::{App, Context, Entity, Global};

/// The sidebar default point size (the 12pt anchor, `FontSettings.swift:38-51`).
pub const DEFAULT_SIDEBAR_FONT_PX: f32 = 12.0;
/// Smallest allowed sidebar size (shares the terminal `[MIN, MAX]` range).
pub const MIN_SIDEBAR_FONT_PX: f32 = 8.0;
/// Largest allowed sidebar size.
pub const MAX_SIDEBAR_FONT_PX: f32 = 32.0;

/// Clamp a sidebar point size into `[MIN, MAX]`.
pub fn clamp_sidebar_px(v: f32) -> f32 {
    v.clamp(MIN_SIDEBAR_FONT_PX, MAX_SIDEBAR_FONT_PX)
}

/// The proportional scale of a sidebar element whose design size is `default_pt`
/// against the 12pt anchor: `max(1, round(sidebar_px * default_pt / 12))` — a
/// direct port of `FontSettings.swift:76-78`'s `sidebarSize(_:)`. Pure (unit-tested).
pub fn sidebar_size(sidebar_px: f32, default_pt: f32) -> f32 {
    (sidebar_px * default_pt / DEFAULT_SIDEBAR_FONT_PX)
        .round()
        .max(1.0)
}

/// The app-level sidebar-font state: the sidebar base px. Constructed via
/// [`SharedSidebarFontSettings::new`] inside `cx.new(...)`.
pub struct SharedSidebarFontSettings {
    /// The sidebar base point size (default 12), clamped to `[MIN, MAX]`.
    px: f32,
}

impl SharedSidebarFontSettings {
    /// A sidebar-font state at `px` (clamped).
    pub fn new(px: f32) -> Self {
        Self {
            px: clamp_sidebar_px(px),
        }
    }

    /// The current sidebar base point size.
    pub fn px(&self) -> f32 {
        self.px
    }

    /// Set the sidebar base px (the Font-pane sidebar stepper + the keyboard zoom).
    /// Clamped; a size that does not move is a no-op (no `notify`).
    pub fn set_px(&mut self, px: f32, cx: &mut Context<Self>) {
        let new = clamp_sidebar_px(px);
        if new != self.px {
            self.px = new;
            cx.notify();
        }
    }

    /// Reset the sidebar to its 12pt default (the Font-pane "Reset to defaults").
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.set_px(DEFAULT_SIDEBAR_FONT_PX, cx);
    }
}

/// The process-level sidebar-font entity Global (the `SharedFontSettings` idiom).
/// Installed by [`crate::keymap::install_shortcuts`] alongside the terminal
/// `SharedFontSettings`; read by the sidebar chrome + the Font pane. Absent ⇒ the
/// sidebar falls back to the 12pt anchor (identity scale) — the isolated scenarios.
pub struct SharedSidebarFont(pub Entity<SharedSidebarFontSettings>);

impl Global for SharedSidebarFont {}

/// The process-level sidebar-font entity, if installed (`None` in isolated
/// scenarios that never install the keymap).
pub(crate) fn shared_sidebar_font(cx: &App) -> Option<Entity<SharedSidebarFontSettings>> {
    cx.try_global::<SharedSidebarFont>().map(|g| g.0.clone())
}

/// The current sidebar base px, or the 12pt default when the entity is absent.
pub(crate) fn current_sidebar_px(cx: &App) -> f32 {
    shared_sidebar_font(cx)
        .map(|e| e.read(cx).px())
        .unwrap_or(DEFAULT_SIDEBAR_FONT_PX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_size_scales_against_the_12pt_anchor() {
        // At the 12pt anchor the scale is identity for any design size.
        assert_eq!(sidebar_size(12.0, 13.0), 13.0);
        assert_eq!(sidebar_size(12.0, 10.0), 10.0);
        // Doubling the sidebar px doubles every element.
        assert_eq!(sidebar_size(24.0, 13.0), 26.0);
        // A non-integer product rounds to nearest.
        assert_eq!(sidebar_size(14.0, 13.0), 15.0); // 14*13/12 = 15.166… → 15
        assert_eq!(sidebar_size(8.0, 13.0), 9.0); // 8*13/12 = 8.666… → 9
    }

    #[test]
    fn sidebar_size_floors_at_one() {
        // A product that rounds to 0 is floored to 1 (never a 0pt element).
        assert_eq!(sidebar_size(1.0, 1.0), 1.0); // 1/12 = 0.083 → round 0 → max(1)
        assert_eq!(sidebar_size(8.0, 0.5), 1.0); // 8*0.5/12 = 0.333 → round 0 → max(1)
    }

    #[test]
    fn clamp_sidebar_px_bounds() {
        assert_eq!(clamp_sidebar_px(12.0), 12.0);
        assert_eq!(clamp_sidebar_px(2.0), MIN_SIDEBAR_FONT_PX);
        assert_eq!(clamp_sidebar_px(99.0), MAX_SIDEBAR_FONT_PX);
    }

    // ---------------------------------------------------------------------
    // Entity-level `#[gpui::test]` on the MOCKED `TestAppContext` (no Metal, no
    // pixels; parallel-safe): `set_px` clamping / notify-only-on-change and
    // `reset`. Lives IN THIS CRATE because `SharedSidebarFontSettings` is
    // app-shaped and a dev/test crate cannot import this binary crate.
    use gpui::{AppContext as _, TestAppContext};
    use std::cell::Cell;
    use std::rc::Rc;

    /// Wire an observer that counts `cx.notify()`s on the sidebar entity, parked
    /// once so the deferred subscription activation is live before the first
    /// mutation (the `font_mutators` idiom).
    fn observe_notifies(
        cx: &mut TestAppContext,
        sidebar: &Entity<SharedSidebarFontSettings>,
    ) -> (Rc<Cell<usize>>, gpui::Subscription) {
        let n = Rc::new(Cell::new(0usize));
        let sub = cx.update(|app| {
            let n = n.clone();
            app.observe(sidebar, move |_, _| n.set(n.get() + 1))
        });
        cx.run_until_parked();
        (n, sub)
    }

    #[gpui::test]
    fn set_px_clamps_no_ops_and_reset_restores_the_default(cx: &mut TestAppContext) {
        let sidebar = cx.new(|_| SharedSidebarFontSettings::new(DEFAULT_SIDEBAR_FONT_PX));
        let (notifies, _sub) = observe_notifies(cx, &sidebar);

        // set_px clamps above MAX / below MIN and notifies on a real move.
        let before = notifies.get();
        sidebar.update(cx, |s, cx| s.set_px(100.0, cx));
        cx.run_until_parked();
        cx.update(|app| assert_eq!(sidebar.read(app).px(), MAX_SIDEBAR_FONT_PX));
        assert!(notifies.get() > before, "set_px notifies on a move");
        sidebar.update(cx, |s, cx| s.set_px(1.0, cx));
        cx.run_until_parked();
        cx.update(|app| assert_eq!(sidebar.read(app).px(), MIN_SIDEBAR_FONT_PX));

        // A set_px that does not move the (already-clamped) size is a no-op: no notify.
        let steady = notifies.get();
        sidebar.update(cx, |s, cx| s.set_px(8.0, cx)); // already at MIN (8)
        cx.run_until_parked();
        assert_eq!(notifies.get(), steady, "a no-move set_px does not notify");

        // reset restores the 12pt default and notifies; a reset already at 12 is a no-op.
        sidebar.update(cx, |s, cx| s.reset(cx));
        cx.run_until_parked();
        cx.update(|app| assert_eq!(sidebar.read(app).px(), DEFAULT_SIDEBAR_FONT_PX));
        let after_reset = notifies.get();
        sidebar.update(cx, |s, cx| s.reset(cx));
        cx.run_until_parked();
        assert_eq!(
            notifies.get(),
            after_reset,
            "reset from the default is a no-op (no notify)"
        );
    }
}
