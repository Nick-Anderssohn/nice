//! R23 shared-`FontSettings` mutator probe — libtest `#[gpui::test]` cases on the
//! mocked [`gpui::TestAppContext`] (no Metal, no pixels; parallel-safe).
//!
//! Proves the parameter-shaped, boundary-legal mutators R23 adds to
//! [`nice_term_view::FontSettings`] (the Font pane's size stepper / family picker /
//! Reset drive these): [`set_px`](nice_term_view::FontSettings::set_px) clamps to
//! `[MIN, MAX]` and `notify`s;
//! [`set_family`](nice_term_view::FontSettings::set_family) re-resolves the chain
//! (`Some` ⇒ that family, `None` ⇒ the default chain); and
//! [`reset_to_defaults`](nice_term_view::FontSettings::reset_to_defaults) returns
//! the size to 13 + the default chain.
//!
//! The probe lives here rather than in `nice-term-view` because that crate has no
//! `test-support` dev-dep — a `#[gpui::test]` / `TestAppContext` there would not
//! compile, while `nice-itests` already imports `nice-term-view` with
//! `test-support`. This case constructs a bare `FontSettings` entity (no pty
//! session), so there is no drain waker to opt out of (gotcha 1 concerns
//! session-backed tests) and `run_until_parked` parks immediately. Asserts field
//! state + notify only — never cadence / perf / wall-clock timing.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{AppContext as _, SharedString, TestAppContext};
use nice_term_view::{
    default_font_chain, FontSettings, DEFAULT_TERMINAL_FONT_PX, MAX_TERMINAL_FONT_PX,
    MIN_TERMINAL_FONT_PX,
};

#[gpui::test]
fn font_mutators_clamp_reresolve_and_reset(cx: &mut TestAppContext) {
    let font = cx.new(FontSettings::resolved_default);

    // Count notifies. Subscription activation is deferred, so park once after
    // wiring before the first mutation.
    let notifies = Rc::new(Cell::new(0usize));
    let _sub = cx.update(|app| {
        let n = notifies.clone();
        app.observe(&font, move |_, _| n.set(n.get() + 1))
    });
    cx.run_until_parked();

    // --- set_px clamps to MAX, notifies -------------------------------------
    let before = notifies.get();
    font.update(cx, |f, cx| f.set_px(100.0, cx));
    cx.run_until_parked();
    cx.update(|app| {
        assert_eq!(
            font.read(app).px(),
            MAX_TERMINAL_FONT_PX,
            "set_px clamps an over-max size to MAX"
        );
    });
    assert!(notifies.get() > before, "set_px fires cx.notify()");

    // A below-min size clamps to MIN.
    font.update(cx, |f, cx| f.set_px(1.0, cx));
    cx.run_until_parked();
    cx.update(|app| {
        assert_eq!(
            font.read(app).px(),
            MIN_TERMINAL_FONT_PX,
            "set_px clamps a below-min size to MIN"
        );
    });

    // --- set_family(Some) re-resolves to that family -----------------------
    font.update(cx, |f, cx| {
        f.set_family(Some(SharedString::from("JetBrains Mono")), cx)
    });
    cx.run_until_parked();
    cx.update(|app| {
        assert_eq!(
            font.read(app).chain(),
            &[SharedString::from("JetBrains Mono")],
            "set_family(Some) makes the given family the sole chain entry"
        );
    });

    // --- set_family(None) restores the default chain -----------------------
    font.update(cx, |f, cx| f.set_family(None, cx));
    cx.run_until_parked();
    cx.update(|app| {
        assert_eq!(
            font.read(app).chain(),
            default_font_chain().as_slice(),
            "set_family(None) restores the shipped default chain"
        );
    });

    // --- reset_to_defaults returns px to 13 + the default chain ------------
    font.update(cx, |f, cx| f.set_px(20.0, cx));
    cx.run_until_parked();
    font.update(cx, |f, cx| f.reset_to_defaults(cx));
    cx.run_until_parked();
    cx.update(|app| {
        let f = font.read(app);
        assert_eq!(
            f.px(),
            DEFAULT_TERMINAL_FONT_PX,
            "reset_to_defaults returns the size to 13"
        );
        assert_eq!(
            f.chain(),
            default_font_chain().as_slice(),
            "reset_to_defaults restores the default chain"
        );
    });
}
