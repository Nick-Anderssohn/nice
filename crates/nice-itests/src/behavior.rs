//! Behavior-context fixtures: boot the **mocked** [`gpui::TestAppContext`], mount
//! a [`TerminalView`] with the `nice-theme` tokens applied, and drive simulated
//! keystrokes / mouse events.
//!
//! This module names test-support-only gpui types, so it is compiled only under
//! `cfg(test)` (this crate's own exemplars) or the `test-support` feature
//! (downstream R9–R13 test crates). It never touches Metal or real pixels — the
//! mocked context uses `TestPlatform` + `NoopTextSystem` — so it is for focus /
//! dispatch / entity-behavior / byte-exact-input tests only, and it may
//! parallelize under libtest. Anything that needs to read pixels back belongs on
//! the visual context (the `harness = false` binaries), not here.
//!
//! The fixed cell metrics the caller passes make the layout deterministic and
//! font-independent (the mocked `NoopTextSystem` derives no metrics), matching
//! how the live renderer self-tests use `FontSettings::fixed`.

use gpui::{div, prelude::*, Context, Entity, Modifiers, Pixels, Point, SharedString, Window};
use gpui::{MouseButton, TestAppContext, VisualTestContext};

use nice_term_view::{FontSettings, TerminalMetrics, TerminalSessionHandle, TerminalTheme, TerminalView};
use nice_theme::AccentPreset;

/// A stock monospace family for the fixed-metrics font state. Font *resolution*
/// is irrelevant on the mocked context (no glyphs are rasterised); the fixed cell
/// box drives all layout, so any installed family name is fine.
const FIXTURE_FONT_FAMILY: &str = "Menlo";
const FIXTURE_FONT_PX: f32 = 13.0;

/// The minimal root view that hosts the terminal under test — `div().size_full()`
/// with the [`TerminalView`] as its only child, so the view fills the window.
struct FixtureRoot {
    terminal: Entity<TerminalView>,
}

impl Render for FixtureRoot {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.terminal.clone())
    }
}

/// Boot a fresh mocked app context. Downstream `#[gpui::test]` cases receive one
/// from the macro; this is for the rare test that wants to build its own.
pub fn boot() -> TestAppContext {
    TestAppContext::single()
}

/// Build a [`TerminalView`] over `handle` with the **`nice-theme` tokens
/// applied** — the Nice default-dark terminal theme + the Terracotta accent — at
/// fixed cell `metrics`. Does not mount it; see [`mount_view`].
pub fn make_terminal(
    cx: &mut TestAppContext,
    handle: Entity<TerminalSessionHandle>,
    metrics: TerminalMetrics,
) -> Entity<TerminalView> {
    // Opt this mocked-context session out of the event-driven drain wake BEFORE
    // the first paint's `run_until_parked` registers the drain's waker: the pty
    // feeder runs on a real OS thread, and waking a gpui task from it trips the
    // deterministic test scheduler's determinism guard. Mocked-context tests never
    // need the drain (they read the grid / capture file directly). See
    // `TerminalSessionHandle::set_event_wake_enabled`.
    handle.update(cx, |h, _cx| h.set_event_wake_enabled(false));
    let theme = TerminalTheme::nice_default_dark();
    let accent = AccentPreset::Terracotta.color();
    let font = cx.new(|_cx| {
        FontSettings::fixed(SharedString::from(FIXTURE_FONT_FAMILY), FIXTURE_FONT_PX, metrics)
    });
    cx.new(|cx| TerminalView::new(handle, theme, accent, font, cx))
}

/// Mount an already-built `terminal` in a fresh mocked window and run to a first
/// paint (which registers the view's key/mouse listeners and takes focus).
/// Returns the [`VisualTestContext`] the caller drives events through. The
/// terminal is owned by the caller (pass a clone here).
pub fn mount_view(
    cx: &mut TestAppContext,
    terminal: Entity<TerminalView>,
) -> &mut VisualTestContext {
    let (_root, vcx) = cx.add_window_view(move |_window, _cx| FixtureRoot { terminal });
    vcx.run_until_parked();
    vcx
}

/// Convenience: [`make_terminal`] + [`mount_view`]. Returns both the view (for
/// reading its state) and the window context (for driving input).
pub fn mount_terminal(
    cx: &mut TestAppContext,
    handle: Entity<TerminalSessionHandle>,
    metrics: TerminalMetrics,
) -> (Entity<TerminalView>, &mut VisualTestContext) {
    let terminal = make_terminal(cx, handle, metrics);
    let vcx = mount_view(cx, terminal.clone());
    (terminal, vcx)
}

/// Simulated keystroke driver: dispatch a space-separated key sequence
/// (`"up"`, `"ctrl-a"`, `"cmd-v enter"`, …) into the focused view via gpui's real
/// key-dispatch path — the same `on_key_down` the live app runs — then run to
/// quiescence.
pub fn press_keys(vcx: &mut VisualTestContext, keys: &str) {
    vcx.simulate_keystrokes(keys);
}

/// Simulated mouse-click driver: a left-button down+up at `position` (logical px,
/// content-view origin) with `modifiers` held, then run to quiescence.
pub fn click(vcx: &mut VisualTestContext, position: Point<Pixels>, modifiers: Modifiers) {
    vcx.simulate_click(position, modifiers);
}

/// Simulated mouse-press driver: a single button-down at `position` (for tests
/// that need to hold a drag; pair with [`release_mouse`]).
pub fn press_mouse(
    vcx: &mut VisualTestContext,
    position: Point<Pixels>,
    button: MouseButton,
    modifiers: Modifiers,
) {
    vcx.simulate_mouse_down(position, button, modifiers);
}

/// Simulated mouse-release driver: a single button-up at `position`.
pub fn release_mouse(
    vcx: &mut VisualTestContext,
    position: Point<Pixels>,
    button: MouseButton,
    modifiers: Modifiers,
) {
    vcx.simulate_mouse_up(position, button, modifiers);
}
