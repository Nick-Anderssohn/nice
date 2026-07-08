//! `SharedSidebarFontSettings` — the app-level sidebar-font state (R23 Binding
//! decision D3, What-to-build item 4).
//!
//! **Boundary (TRANCHE-2-NOTES §4, binding):** sidebar font size is NOT a terminal
//! concept, so it must NOT live in `nice-term-view::FontSettings`. It lives here,
//! in an app-level gpui entity in `crates/nice`, mirroring the `SharedFontSettings`
//! Global idiom (`keymap.rs`). It holds the sidebar base point size + the ported
//! [`sidebar_size`] proportional-scale helper the sidebar chrome reads.
//!
//! ## Coupled to terminal zoom (Swift parity)
//! The entity SUBSCRIBES to the shared terminal [`FontSettings`]' [`FontZoom`]
//! event (the surface `font.rs` emits expressly for this proportional subscriber):
//! on a terminal zoom (⌘=/⌘−/⌘0 or the Font-pane size slider) it ratio-preserving
//! rescales the sidebar px (`clamp(round(sidebar_px × new_px / old_px))`), matching
//! Swift's `⌘=`/`⌘−`/`⌘0` scaling BOTH sizes (`FontSettings.swift:91-105`). The
//! Font-pane's "Reset to defaults" resets the sidebar explicitly via [`reset`]
//! (which is why `FontSettings::reset_to_defaults` does NOT emit `FontZoom` — a
//! proportional rescale off it would fight the explicit reset).

use gpui::{App, Context, Entity, Global, Subscription};

use nice_term_view::{FontSettings, FontZoom};

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

/// The app-level sidebar-font state: the sidebar base px + the tracked terminal px
/// (the ratio reference for the proportional [`FontZoom`] rescale). Constructed via
/// [`SharedSidebarFontSettings::new`] inside `cx.new(...)`.
pub struct SharedSidebarFontSettings {
    /// The sidebar base point size (default 12), clamped to `[MIN, MAX]`.
    px: f32,
    /// The terminal px last seen through the [`FontZoom`] subscription — the
    /// denominator of the proportional rescale ratio.
    last_terminal_px: f32,
    /// The terminal-zoom subscription (proportional rescale). Held so it lives as
    /// long as the entity.
    _zoom_sub: Subscription,
}

impl SharedSidebarFontSettings {
    /// A sidebar-font state at `px`, coupled to `font`'s [`FontZoom`] with
    /// `terminal_px` as the initial ratio reference (the terminal's px at
    /// construction). Call inside `cx.new(|cx| SharedSidebarFontSettings::new(...))`.
    pub fn new(
        px: f32,
        terminal_px: f32,
        font: &Entity<FontSettings>,
        cx: &mut Context<Self>,
    ) -> Self {
        let sub = cx.subscribe(font, |this, _font, ev: &FontZoom, cx| {
            this.on_terminal_zoom(ev.px, cx);
        });
        Self {
            px: clamp_sidebar_px(px),
            last_terminal_px: terminal_px,
            _zoom_sub: sub,
        }
    }

    /// The current sidebar base point size.
    pub fn px(&self) -> f32 {
        self.px
    }

    /// Set the sidebar base px (the Font-pane sidebar-size slider). Clamped; a
    /// size that does not move is a no-op (no `notify`).
    pub fn set_px(&mut self, px: f32, cx: &mut Context<Self>) {
        let new = clamp_sidebar_px(px);
        if new != self.px {
            self.px = new;
            cx.notify();
        }
    }

    /// Reset the sidebar to its 12pt default (the Font-pane "Reset to defaults").
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        if self.px != DEFAULT_SIDEBAR_FONT_PX {
            self.px = DEFAULT_SIDEBAR_FONT_PX;
            cx.notify();
        }
    }

    /// Proportionally rescale off a terminal zoom to `new_terminal_px`
    /// (`clamp(round(sidebar_px × new / old))`), then advance the ratio reference.
    fn on_terminal_zoom(&mut self, new_terminal_px: f32, cx: &mut Context<Self>) {
        if self.last_terminal_px <= 0.0 {
            self.last_terminal_px = new_terminal_px;
            return;
        }
        let ratio = new_terminal_px / self.last_terminal_px;
        self.last_terminal_px = new_terminal_px;
        let scaled = clamp_sidebar_px((self.px * ratio).round());
        if scaled != self.px {
            self.px = scaled;
            cx.notify();
        }
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
    // Entity-level `#[gpui::test]`s on the MOCKED `TestAppContext` (no Metal,
    // no pixels; parallel-safe) — the stateful D3 coupling the pure helpers
    // above cannot reach: the terminal-zoom proportional rescale
    // (`on_terminal_zoom`), the sidebar-slider `set_px`, and `reset`.
    //
    // These live IN THIS CRATE (not `nice-itests`) because `SharedSidebarFontSettings`
    // is app-shaped and a dev/test crate cannot import this binary crate — the
    // same constraint the R9 `chrome_band` / R10 `sidebar_multiselect` probes
    // have. `nice`'s `gpui/test-support` dev-dep compiles `#[gpui::test]` here.
    use gpui::{AppContext as _, TestAppContext};
    use nice_term_view::DEFAULT_TERMINAL_FONT_PX;
    use std::cell::Cell;
    use std::rc::Rc;

    /// Wire an observer that counts `cx.notify()`s on the sidebar entity, parked
    /// once so the deferred subscription activations are live before the first
    /// emitting mutation (the `font_mutators` idiom).
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
    fn terminal_zoom_rescales_the_sidebar_proportionally(cx: &mut TestAppContext) {
        let font = cx.new(FontSettings::resolved_default); // terminal px = 13
        let sidebar = cx.new(|cx| {
            SharedSidebarFontSettings::new(DEFAULT_SIDEBAR_FONT_PX, DEFAULT_TERMINAL_FONT_PX, &font, cx)
        });
        let (notifies, _sub) = observe_notifies(cx, &sidebar);

        // Doubling the terminal px doubles the sidebar px (ratio 26/13 = 2 → 12*2).
        let before = notifies.get();
        font.update(cx, |f, cx| f.set_px(26.0, cx));
        cx.run_until_parked();
        cx.update(|app| {
            assert_eq!(
                sidebar.read(app).px(),
                24.0,
                "a terminal zoom to 2× rescales the sidebar to 2× (12 → 24)"
            );
        });
        assert!(notifies.get() > before, "a rescale that moves the size notifies");

        // Halving the reference (26 → 13) halves the sidebar back (24 → 12); the
        // reference advanced on the previous zoom, so this ratio is 13/26 = 0.5.
        font.update(cx, |f, cx| f.set_px(13.0, cx));
        cx.run_until_parked();
        cx.update(|app| {
            assert_eq!(
                sidebar.read(app).px(),
                12.0,
                "the ratio reference advanced: a return to 13 restores 12"
            );
        });

        // A FontZoom that does NOT move the terminal px (a family change emits
        // `FontZoom { px: self.px }`, px unchanged) is a proportional no-op: ratio
        // 1 leaves the sidebar at 12 and fires NO notify (notify-only-if-changed).
        let steady = notifies.get();
        font.update(cx, |f, cx| f.set_family(Some("JetBrains Mono".into()), cx));
        cx.run_until_parked();
        cx.update(|app| assert_eq!(sidebar.read(app).px(), 12.0));
        assert_eq!(
            notifies.get(),
            steady,
            "a same-size FontZoom rescales to the same px and does not notify"
        );
    }

    #[gpui::test]
    fn terminal_zoom_clamps_the_rescaled_sidebar(cx: &mut TestAppContext) {
        let font = cx.new(FontSettings::resolved_default);
        // A reference of 8 with a sidebar base of 20 forces the clamp: a zoom to 26
        // is ratio 26/8 = 3.25 → 20*3.25 = 65, clamped to MAX (32).
        let sidebar = cx.new(|cx| SharedSidebarFontSettings::new(20.0, 8.0, &font, cx));
        let (_notifies, _sub) = observe_notifies(cx, &sidebar);

        font.update(cx, |f, cx| f.set_px(26.0, cx));
        cx.run_until_parked();
        cx.update(|app| {
            assert_eq!(
                sidebar.read(app).px(),
                MAX_SIDEBAR_FONT_PX,
                "an over-max rescale clamps the sidebar to MAX (32)"
            );
        });

        // A ratio that would drive it below MIN clamps to MIN: reference 26,
        // sidebar 32 → a zoom to 8 is ratio 8/26 ≈ 0.31 → 32*0.31 ≈ 9.8 → 10... use
        // a fresh, lower base to reach the floor deterministically.
        let sidebar2 = cx.new(|cx| SharedSidebarFontSettings::new(9.0, 26.0, &font, cx));
        let (_n2, _s2) = observe_notifies(cx, &sidebar2);
        // font is currently at 26 (from the clamp step). Drive it to 8: ratio 8/26,
        // 9*8/26 ≈ 2.77 → round 3 → clamp MIN (8).
        font.update(cx, |f, cx| f.set_px(8.0, cx));
        cx.run_until_parked();
        cx.update(|app| {
            assert_eq!(
                sidebar2.read(app).px(),
                MIN_SIDEBAR_FONT_PX,
                "a below-min rescale clamps the sidebar to MIN (8)"
            );
        });
    }

    #[gpui::test]
    fn zero_reference_guard_advances_without_rescaling(cx: &mut TestAppContext) {
        let font = cx.new(FontSettings::resolved_default); // px = 13
        // A non-positive reference (the `last_terminal_px <= 0.0` guard, line 105):
        // the first zoom must ONLY adopt the incoming px as the reference — never
        // divide by it — leaving the sidebar untouched.
        let sidebar = cx.new(|cx| SharedSidebarFontSettings::new(DEFAULT_SIDEBAR_FONT_PX, 0.0, &font, cx));
        let (notifies, _sub) = observe_notifies(cx, &sidebar);

        let before = notifies.get();
        font.update(cx, |f, cx| f.set_px(26.0, cx)); // FontZoom { px: 26 }
        cx.run_until_parked();
        cx.update(|app| {
            assert_eq!(
                sidebar.read(app).px(),
                DEFAULT_SIDEBAR_FONT_PX,
                "the zero-reference guard leaves the sidebar unchanged on the first zoom"
            );
        });
        assert_eq!(notifies.get(), before, "the guard path does not notify");

        // The reference is now 26 (adopted). A subsequent zoom back to 13 rescales
        // proportionally (13/26 = 0.5 → 12*0.5 = 6 → clamped to MIN 8), proving the
        // reference was advanced by the guard rather than the zoom being lost.
        font.update(cx, |f, cx| f.set_px(13.0, cx));
        cx.run_until_parked();
        cx.update(|app| {
            assert_eq!(
                sidebar.read(app).px(),
                MIN_SIDEBAR_FONT_PX,
                "after the guard adopted 26, a zoom to 13 rescales (and clamps to MIN)"
            );
        });
    }

    #[gpui::test]
    fn set_px_clamps_no_ops_and_reset_restores_the_default(cx: &mut TestAppContext) {
        let font = cx.new(FontSettings::resolved_default);
        let sidebar = cx.new(|cx| {
            SharedSidebarFontSettings::new(DEFAULT_SIDEBAR_FONT_PX, DEFAULT_TERMINAL_FONT_PX, &font, cx)
        });
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
