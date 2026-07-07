//! R21 terminal-view live-recolor setters — libtest `#[gpui::test]` cases on the
//! mocked [`gpui::TestAppContext`] (no Metal, no pixels; parallel-safe).
//!
//! Proves the boundary-legal fan-out seam the app pushes theme changes through
//! ([`nice_term_view::TerminalView::set_theme`] / [`set_accent`]): each replaces
//! the private render field(s) and fires `cx.notify()` for a repaint with **no
//! view rebuild** (the same shape as the font-zoom `on_font_changed` path), and an
//! accent-only change recolors the caret on a `cursor: None` theme (where the
//! block caret paints in the accent). The probe lives here rather than in
//! `nice-term-view` because that crate has no test harness — `nice-itests` is
//! where the view is driven under a `TestAppContext`.
//!
//! [`set_accent`]: nice_term_view::TerminalView::set_accent
//! Neither case asserts cadence / perf / wall-clock timing.

use std::cell::Cell;
use std::rc::Rc;

use gpui::TestAppContext;
use nice_term_core::DEFAULT_SCROLLBACK_LINES;
use nice_term_view::{TerminalMetrics, TerminalSessionHandle, TerminalTheme};
use nice_theme::AccentPreset;

use crate::{behavior, session};

/// Fixed cell box for the fixture (font-independent, like the renderer self-tests'
/// `FontSettings::fixed`).
const CELL_W: f32 = 8.0;
const CELL_H: f32 = 16.0;

/// `set_theme` replaces both the render theme and the accent; `set_accent`
/// replaces only the accent (recoloring the caret on a `cursor: None` theme). Each
/// fires exactly the notify a repaint needs, with no rebuild.
#[gpui::test]
fn set_theme_and_set_accent_recolor_without_rebuild(cx: &mut TestAppContext) {
    let dir = session::temp_dir("theme-setters").expect("temp dir");
    // A silent session only to construct the view — the setters exercise pure
    // field state, no pty I/O. `make_terminal` opts this mocked session out of the
    // event-driven drain wake BEFORE the first `run_until_parked` (gotcha 1), and
    // the blocked `cat` child is reaped when the session entity drops at test end.
    let spec = session::silent_command_spec(&dir, "cat", 24, 80);
    let handle = TerminalSessionHandle::spawn(cx, spec, DEFAULT_SCROLLBACK_LINES).expect("spawn");
    let terminal =
        behavior::make_terminal(cx, handle.clone(), TerminalMetrics::new(CELL_W, CELL_H));

    // Baseline: `make_terminal`'s seed — the Nice default-dark theme + Terracotta.
    // The Nice defaults leave the cursor unset, so the block caret follows the
    // accent (the precondition for the accent-only leg below).
    cx.update(|app| {
        let v = terminal.read(app);
        assert_eq!(*v.theme(), TerminalTheme::nice_default_dark());
        assert_eq!(v.accent(), AccentPreset::Terracotta.color());
        assert_eq!(v.theme().cursor, None, "the Nice default leaves the caret on the accent");
    });

    // Observe notifies so each setter is proven to repaint. Assert monotonic
    // growth (not an exact count) so an incidental handle notify can't flake it.
    let notifies = Rc::new(Cell::new(0usize));
    let _sub = cx.update(|cx| {
        let n = notifies.clone();
        cx.observe(&terminal, move |_, _| n.set(n.get() + 1))
    });

    // `set_theme` swaps BOTH the render theme and the accent, and notifies.
    let new_theme = TerminalTheme::nice_default_light();
    let new_accent = AccentPreset::Ocean.color();
    let before = notifies.get();
    terminal.update(cx, |v, cx| v.set_theme(new_theme.clone(), new_accent, cx));
    cx.run_until_parked();
    cx.update(|app| {
        let v = terminal.read(app);
        assert_eq!(*v.theme(), new_theme, "set_theme replaces the render theme");
        assert_eq!(v.accent(), new_accent, "set_theme replaces the accent");
    });
    assert!(notifies.get() > before, "set_theme fires cx.notify()");

    // `set_accent` recolors ONLY the accent — the theme is untouched, so on this
    // `cursor: None` theme it is the caret recolor.
    let caret_accent = AccentPreset::Fern.color();
    let before = notifies.get();
    terminal.update(cx, |v, cx| v.set_accent(caret_accent, cx));
    cx.run_until_parked();
    cx.update(|app| {
        let v = terminal.read(app);
        assert_eq!(v.accent(), caret_accent, "set_accent replaces the accent");
        assert_eq!(*v.theme(), new_theme, "set_accent leaves the render theme unchanged");
        assert_eq!(v.theme().cursor, None, "the caret still follows the (new) accent");
    });
    assert!(notifies.get() > before, "set_accent fires cx.notify()");
}
