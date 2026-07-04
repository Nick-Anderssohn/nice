//! In-process band-press classification differentials for the R9 window-chrome
//! band — **execution model: mocked [`gpui::TestAppContext`], ordinary libtest
//! `#[gpui::test]` cases** (no Metal, no pixels; parallel-safe).
//!
//! The shipped band (`WindowChromeView` in the `nice` binary) cannot be imported
//! here: `nice-itests` is dev/test-only and the app binary never depends on it
//! (and vice versa). So this mirrors the band's press-arbitration handlers in a
//! local [`ChromeBandProbe`] view that **records what the band would do** — arm a
//! press, start a window move, run the double-click action — instead of calling
//! the real [`gpui::Window`] methods the band calls (`start_window_move` /
//! `titlebar_double_click`), which need a real NSWindow the mocked context does
//! not have. What these cases verify is the GPUI event-propagation **contract**
//! the band relies on and that R10/R11's interactive children reuse:
//!
//!   * a child that consumes its own press (`stop_propagation`) keeps the band
//!     handler from ever seeing it (the differential-pair rule: the child works
//!     AND the window did not move);
//!   * a double-click on the empty band is consumed in every case;
//!   * the ~2pt drag threshold classifies press-then-move into a window move,
//!     and only a press that actually reached the band can arm one.
//!
//! The real `Window`-method effects (an NSWindow frame that moves, the user's
//! `AppleActionOnDoubleClick`, the full-screen gate) are ground-truthed by the
//! live `chrome` self-test scenario. Neither this nor any behavior test asserts
//! cadence / perf / wall-clock timing — those live only in the live suite.

use gpui::{
    div, point, prelude::*, px, App, Context, Entity, IntoElement, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, TestAppContext, Window,
};

use nice_theme::chrome_geometry::TOP_BAR_HEIGHT;

/// The ~2pt window-drag threshold, squared — mirrors `nice`'s
/// `BAND_DRAG_THRESHOLD_PX` (2.0) compared as `dx*dx + dy*dy >= 4`
/// (`ChromeEventRouter.swift:218`). Kept in sync by hand because the private
/// `nice` constant can't be imported; `nice`'s own unit test pins the same 4.
const DRAG_THRESHOLD_SQ: f32 = 4.0;

/// Stub interactive-child geometry inside the band — a stand-in for an R10 toggle
/// / R11 pill until those land real ones. A small rect near the leading edge,
/// comfortably inside the 52pt band (`CHILD_Y + CHILD_H = 43 < 52`).
const CHILD_X: f32 = 40.0;
const CHILD_Y: f32 = 9.0;
const CHILD_W: f32 = 60.0;
const CHILD_H: f32 = 34.0;

/// A point inside the stub child (its centre).
fn child_center() -> Point<Pixels> {
    point(px(CHILD_X + CHILD_W / 2.0), px(CHILD_Y + CHILD_H / 2.0))
}

/// A point on the empty band, clear of the child and well inside the maximized
/// test window's width (`TestPlatform`'s display is 1920×1080), on the y-26 row.
fn empty_band_point() -> Point<Pixels> {
    point(px(800.0), px(TOP_BAR_HEIGHT / 2.0))
}

/// The probe: a full-window background catcher, a full-width 52pt band carrying
/// the empty-chrome press handlers (mirrors of `WindowChromeView`'s), and one
/// stub interactive child inside the band. Every handler records an outcome
/// instead of touching a real `Window`.
#[derive(Default)]
struct ChromeBandProbe {
    /// The single in-flight band press origin — the ONLY remembered state, like
    /// `WindowChromeView::band_press` (never a cross-element flag).
    band_press: Option<Point<Pixels>>,
    /// Band single-press arms (a press that reached the band and armed a drag).
    band_presses: u32,
    /// Band double-click actions taken (each consumes the event).
    band_double_clicks: u32,
    /// Band drags that crossed the threshold — where the band would call
    /// `window.start_window_move()`.
    band_window_moves: u32,
    /// Presses that reached the stub child (which consumes them).
    child_presses: u32,
    /// Presses that bubbled all the way to the window-background catcher, i.e.
    /// were NOT consumed by the band or the child — observes non-consumption.
    escaped_to_background: u32,
}

impl ChromeBandProbe {
    /// Mirror of `WindowChromeView::on_band_mouse_down`: in full screen pass the
    /// press through; a double-click runs the (recorded) title-bar action and is
    /// consumed; a single press arms a potential window drag from this point.
    fn on_band_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.band_press = None;
        if window.is_fullscreen() {
            return;
        }
        if event.click_count >= 2 {
            self.band_double_clicks += 1;
            cx.stop_propagation();
            return;
        }
        self.band_presses += 1;
        self.band_press = Some(event.position);
    }

    /// Mirror of `WindowChromeView::on_band_mouse_move`: once an armed press
    /// leaves the ~2pt threshold with the left button held, the band would hand
    /// the drag to AppKit (`start_window_move`); a move without the left button
    /// disarms.
    fn on_band_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        let Some(origin) = self.band_press else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.band_press = None;
            return;
        }
        let dx = f32::from(event.position.x - origin.x);
        let dy = f32::from(event.position.y - origin.y);
        if dx * dx + dy * dy >= DRAG_THRESHOLD_SQ {
            self.band_press = None;
            self.band_window_moves += 1;
        }
    }

    /// Mirror of `WindowChromeView::on_band_mouse_up`: disarm any pending drag.
    fn on_band_up(&mut self, _event: &MouseUpEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        self.band_press = None;
    }

    /// The interactive-child contract R10/R11 follow: consume the press so it
    /// never reaches the band.
    fn on_child_down(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.child_presses += 1;
        cx.stop_propagation();
    }

    /// The outermost catcher — fires only for presses neither the band nor the
    /// child consumed, so it observes non-consumption.
    fn on_background_down(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.escaped_to_background += 1;
    }
}

impl Render for ChromeBandProbe {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            // Outermost background catcher, painted first so it is LAST in the
            // bubble phase — it sees a press only if nothing inner consumed it.
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_background_down))
            .child(
                // The full-width 52pt band with the empty-chrome press handlers.
                div()
                    .relative()
                    .w_full()
                    .h(px(TOP_BAR_HEIGHT))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_band_down))
                    .on_mouse_move(cx.listener(Self::on_band_move))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::on_band_up))
                    .child(
                        // The stub interactive child that consumes its own press.
                        div()
                            .absolute()
                            .left(px(CHILD_X))
                            .top(px(CHILD_Y))
                            .w(px(CHILD_W))
                            .h(px(CHILD_H))
                            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_child_down)),
                    ),
            )
    }
}

/// Mount a fresh probe in a maximized mocked window and run to a first paint (so
/// the hitboxes + listeners are registered). Returns the view (to read outcomes)
/// and the window context (to drive events).
fn mount_probe(cx: &mut TestAppContext) -> (Entity<ChromeBandProbe>, &mut gpui::VisualTestContext) {
    let (probe, vcx) = cx.add_window_view(|_window, _cx| ChromeBandProbe::default());
    vcx.run_until_parked();
    (probe, vcx)
}

/// A left mouse-down carrying `click_count` at `position` (the simulate helpers
/// hardcode `click_count: 1`, so a double-click needs the raw event).
fn left_down(position: Point<Pixels>, click_count: usize) -> MouseDownEvent {
    MouseDownEvent {
        position,
        button: MouseButton::Left,
        modifiers: Modifiers::none(),
        click_count,
        first_mouse: false,
    }
}

/// Read one `u32` field of the probe.
fn read_u32(
    probe: &Entity<ChromeBandProbe>,
    vcx: &mut gpui::VisualTestContext,
    f: impl Fn(&ChromeBandProbe) -> u32,
) -> u32 {
    vcx.read(|app: &App| f(probe.read(app)))
}

/// A press on the EMPTY band arms a drag (reaches the band) and, being a single
/// press, is NOT consumed — it bubbles to the background catcher; the child never
/// sees it.
#[gpui::test]
fn single_press_on_empty_band_reaches_band_and_bubbles(cx: &mut TestAppContext) {
    let (probe, vcx) = mount_probe(cx);

    vcx.simulate_mouse_down(empty_band_point(), MouseButton::Left, Modifiers::none());

    assert_eq!(read_u32(&probe, vcx, |p| p.band_presses), 1, "band armed the press");
    assert_eq!(read_u32(&probe, vcx, |p| p.child_presses), 0, "child untouched");
    assert_eq!(
        read_u32(&probe, vcx, |p| p.escaped_to_background),
        1,
        "a single band press is not consumed — it bubbles past the band"
    );
    assert_eq!(
        read_u32(&probe, vcx, |p| p.band_double_clicks),
        0,
        "a single press is not a double-click"
    );
}

/// A DOUBLE-click on the empty band runs the band's title-bar action and is
/// consumed in every case — it never reaches the background catcher.
#[gpui::test]
fn double_click_on_empty_band_is_consumed(cx: &mut TestAppContext) {
    let (probe, vcx) = mount_probe(cx);

    vcx.simulate_event(left_down(empty_band_point(), 2));

    assert_eq!(
        read_u32(&probe, vcx, |p| p.band_double_clicks),
        1,
        "the band ran its double-click action"
    );
    assert_eq!(
        read_u32(&probe, vcx, |p| p.escaped_to_background),
        0,
        "the double-click was consumed — it did not bubble to the background"
    );
    assert_eq!(
        read_u32(&probe, vcx, |p| p.band_presses),
        0,
        "a double-click does not arm a single-press drag"
    );
}

/// A press on the stub INTERACTIVE CHILD is consumed there and never reaches the
/// band (the arbitration convention R10/R11 build on). The differential pair: the
/// child worked AND the band saw nothing.
#[gpui::test]
fn press_on_interactive_child_never_reaches_band(cx: &mut TestAppContext) {
    let (probe, vcx) = mount_probe(cx);

    vcx.simulate_mouse_down(child_center(), MouseButton::Left, Modifiers::none());

    assert_eq!(read_u32(&probe, vcx, |p| p.child_presses), 1, "the child consumed its own press");
    assert_eq!(
        read_u32(&probe, vcx, |p| p.band_presses),
        0,
        "the press never reached the band"
    );
    assert_eq!(
        read_u32(&probe, vcx, |p| p.band_double_clicks),
        0,
        "the band ran no action on the child's press"
    );
    assert_eq!(
        read_u32(&probe, vcx, |p| p.escaped_to_background),
        0,
        "the child consumed the press before the background catcher"
    );
}

/// An empty-band press then a move PAST the ~2pt threshold classifies as a window
/// move; a move UNDER the threshold does not.
#[gpui::test]
fn empty_band_drag_past_threshold_starts_a_window_move(cx: &mut TestAppContext) {
    let (probe, vcx) = mount_probe(cx);
    let start = empty_band_point();

    // Under the threshold (dx=1 → 1 < 4): still armed, no window move.
    vcx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    vcx.simulate_mouse_move(
        point(start.x + px(1.0), start.y),
        Some(MouseButton::Left),
        Modifiers::none(),
    );
    assert_eq!(
        read_u32(&probe, vcx, |p| p.band_window_moves),
        0,
        "a sub-threshold move must not start a window move"
    );

    // Past the threshold (dx=40 → 1600 >= 4): the band would start_window_move.
    vcx.simulate_mouse_move(
        point(start.x + px(40.0), start.y),
        Some(MouseButton::Left),
        Modifiers::none(),
    );
    assert_eq!(
        read_u32(&probe, vcx, |p| p.band_window_moves),
        1,
        "crossing the ~2pt threshold starts exactly one window move"
    );
    vcx.simulate_mouse_up(
        point(start.x + px(40.0), start.y),
        MouseButton::Left,
        Modifiers::none(),
    );
}

/// A drag that STARTS on the interactive child does not move the window: the
/// child consumes the press, so the band never arms, so a subsequent past-
/// threshold move finds nothing to promote (the drag differential's child half).
#[gpui::test]
fn drag_started_on_child_does_not_move_the_window(cx: &mut TestAppContext) {
    let (probe, vcx) = mount_probe(cx);
    let start = child_center();

    vcx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    // Move well past the threshold, but the band never armed a press.
    vcx.simulate_mouse_move(
        point(start.x + px(40.0), start.y),
        Some(MouseButton::Left),
        Modifiers::none(),
    );

    assert_eq!(read_u32(&probe, vcx, |p| p.child_presses), 1, "the child took the press");
    assert_eq!(
        read_u32(&probe, vcx, |p| p.band_window_moves),
        0,
        "a drag beginning on the child never moves the window"
    );
    assert_eq!(
        read_u32(&probe, vcx, |p| p.band_presses),
        0,
        "the band never armed a press from the child's drag"
    );
    vcx.simulate_mouse_up(
        point(start.x + px(40.0), start.y),
        MouseButton::Left,
        Modifiers::none(),
    );
}
