//! `StatusDot` — the sidebar/toolbar status dot, ported from
//! `Sources/Nice/Views/StatusDot.swift`. An 8pt circle whose colour maps to a
//! [`TabStatus`], with two repeat-forever pulse animations: an expanding outer
//! ring and a breathing inner dot. Rendered by slice 3's sidebar `TabRow` and
//! reused by R11's toolbar pills.
//!
//! ## Reads R8, never recomputes (binding decision)
//!
//! The dot's state is supplied by the caller from the R8 model predicates — it
//! is **not** recomputed here:
//!
//!   * [`StatusDot::status`] comes from [`nice_model::Tab::status`] (or, for
//!     R11's per-pane pills, [`nice_model::Pane::status`]);
//!   * [`StatusDot::suppress_waiting_pulse`] comes from
//!     [`nice_model::Tab::waiting_acknowledged`] (per-pane:
//!     [`nice_model::Pane::waiting_acknowledged`]).
//!
//! The *writer* of acknowledgment-on-view is the session/focus logic landing in
//! R13; this component only renders pulse-suppression off the fields as they
//! are. The pulse rule ([`status_dot_should_pulse`]) is: **thinking** always
//! pulses, **waiting** pulses until acknowledged, **idle** never
//! (`StatusDot.swift:47-53`).
//!
//! ## Colours (binding decision)
//!
//! Per-status base colour ([`status_dot_base_color`]): **thinking** = the fixed
//! [`nice_theme::status::THINKING_DOT`] brand Terracotta, **waiting** = the fixed
//! [`nice_theme::status::WAITING_DOT`] blue, **idle** = the active palette's
//! `ink3`. Thinking and waiting are hardcoded tokens read straight here — Swift's
//! `StatusDot` painted thinking in the fixed `.niceAccent`, **not** the user's
//! chosen swatch — so only the palette-dependent `idle` colour is resolved by the
//! caller and passed in, keeping the component reusable across palettes.

// The component's constructor + builder setters have no in-crate caller until
// slice 3 (the sidebar `TabRow`) and R11 (toolbar pills) wire them; it is a
// deliberately-exported reusable component (plan "Exported contracts"). The pure
// colour/pulse helpers below ARE exercised by this module's unit tests.
#![allow(dead_code)]

use std::time::Duration;

use gpui::{
    bounce, canvas, div, ease_in_out, point, px, App, Animation, AnimationExt, Bounds, ElementId,
    IntoElement, ParentElement, PathBuilder, Pixels, RenderOnce, Rgba, Styled, Window,
};

use nice_model::TabStatus;
use nice_theme::color::Srgba;
use nice_theme::status::{
    BreathePulse, RingPulse, BREATHE_MAX_OPACITY, DOT_FRAME_PADDING, DOT_SIZE, THINKING_BREATHE,
    THINKING_DOT, THINKING_RING, WAITING_BREATHE, WAITING_DOT, WAITING_RING,
};

use crate::theme::srgba_to_rgba;

/// The base (unanimated) dot colour for `status`, given the caller-resolved
/// palette-dependent `idle` colour. Ported from `StatusDot.baseColor`
/// (`StatusDot.swift:27-37`): thinking → the fixed [`THINKING_DOT`] brand
/// Terracotta (`.niceAccent`, NOT the user's chosen accent), waiting → the fixed
/// [`WAITING_DOT`] blue, idle → `idle` (the palette's `ink3`).
pub(crate) fn status_dot_base_color(status: TabStatus, idle: Srgba) -> Srgba {
    match status {
        TabStatus::Thinking => THINKING_DOT,
        TabStatus::Waiting => WAITING_DOT,
        TabStatus::Idle => idle,
    }
}

/// Whether the dot pulses for `status`. Ported from `StatusDot.shouldPulse`
/// (`StatusDot.swift:47-53`): thinking always pulses, waiting pulses until
/// acknowledged (`suppress_waiting_pulse`), idle never.
pub(crate) fn status_dot_should_pulse(status: TabStatus, suppress_waiting_pulse: bool) -> bool {
    match status {
        TabStatus::Thinking => true,
        TabStatus::Waiting => !suppress_waiting_pulse,
        TabStatus::Idle => false,
    }
}

/// The ring + breathe pulse specs for a pulsing `status`. Only `thinking` and
/// `waiting` pulse (idle never reaches here); returns the per-status
/// (`StatusDot.swift:94-99,107-111`) constants.
fn pulse_specs(status: TabStatus) -> (RingPulse, BreathePulse) {
    match status {
        TabStatus::Waiting => (WAITING_RING, WAITING_BREATHE),
        // Thinking (and the idle fallthrough, which never pulses) use the
        // thinking timings.
        TabStatus::Thinking | TabStatus::Idle => (THINKING_RING, THINKING_BREATHE),
    }
}

/// A quadratic ease-out (`1 − (1 − t)²`) — a close, dependency-free stand-in for
/// SwiftUI's `.easeOut` used by the expanding ring (`StatusDot.swift:99`). gpui's
/// stock `ease_out_quint` is far steeper, so a local quadratic is the faithful
/// choice.
fn ease_out_quadratic(t: f32) -> f32 {
    let inv = 1.0 - t;
    1.0 - inv * inv
}

/// Paint the expanding pulse ring for one animation frame as an **antialiased
/// filled circle** (`gpui::Window::paint_path`), centred on `bounds`.
///
/// Why a path and not a `rounded_full` `div`: gpui snaps every quad's edges to
/// the device-pixel grid independently (`Window::snap_bounds` →
/// `round_to_device_pixel`). A circle whose `left` and `width` both animate has
/// its two edges cross pixel boundaries at different phases, so the snapped
/// centre drifts up to a device pixel each frame — a rhythmic left/right shake.
/// `paint_path` is **not** snapped, so the circle grows with sub-pixel
/// antialiasing and stays visually centred: smooth, not shaky.
///
/// `delta` is the eased animation progress `0 → 1`: the ring scales
/// `1.0 → max_scale` while its alpha fades `start_opacity → 0`
/// (`StatusDot.swift:91-102`).
fn paint_pulse_ring(
    bounds: Bounds<Pixels>,
    color: Rgba,
    delta: f32,
    ring: RingPulse,
    window: &mut Window,
) {
    let alpha = ring.start_opacity * (1.0 - delta);
    if alpha <= 0.0 {
        return;
    }
    let scale = 1.0 + delta * (ring.max_scale - 1.0);
    let center = bounds.center();
    // `bounds` is the `frame`-sized box, so the base radius is half its width.
    let radius = bounds.size.width * (0.5 * scale);
    let radii = point(radius, radius);
    let left = point(center.x - radius, center.y);
    let right = point(center.x + radius, center.y);

    // A full circle as two semicircular arcs (left → right → left).
    let mut pb = PathBuilder::fill();
    pb.move_to(left);
    pb.arc_to(radii, px(0.0), false, true, right);
    pb.arc_to(radii, px(0.0), false, true, left);
    pb.close();
    if let Ok(path) = pb.build() {
        window.paint_path(
            path,
            Rgba {
                a: color.a * alpha,
                ..color
            },
        );
    }
}

/// The status dot component. Construct with [`StatusDot::new`]; only the
/// palette-dependent `idle` colour (the palette's `ink3`) is resolved by the
/// caller — thinking and waiting are fixed tokens. See the module docs for the
/// "reads R8, never recomputes" rule.
#[derive(IntoElement)]
pub(crate) struct StatusDot {
    /// Unique element-id seed so each dot's per-layer animation state is
    /// distinct even when many dots render as siblings (one per sidebar row).
    id: ElementId,
    status: TabStatus,
    /// Idle colour — the active palette's `ink3` slot.
    idle: Srgba,
    /// Inner-dot diameter (pt). Defaults to [`DOT_SIZE`] (8).
    size: f32,
    /// Fed from the R8 `waiting_acknowledged` predicate: suppresses the
    /// **waiting** pulse when the user is already looking at the owning tab/pane.
    suppress_waiting_pulse: bool,
    /// Disables the pulse entirely (for snapshots / non-animated previews),
    /// mirroring Swift's `pulsePaused` (`StatusDot.swift:21`).
    pulse_paused: bool,
}

impl StatusDot {
    /// A status dot for `status`, with the caller-resolved `idle` (the palette's
    /// `ink3`) colour — thinking and waiting are fixed tokens. `id` seeds the
    /// per-layer animation element-ids and must be unique among sibling dots
    /// (e.g. the tab id). Size defaults to [`DOT_SIZE`]; the pulse flags default
    /// off.
    pub(crate) fn new(id: impl Into<ElementId>, status: TabStatus, idle: Srgba) -> Self {
        Self {
            id: id.into(),
            status,
            idle,
            size: DOT_SIZE,
            suppress_waiting_pulse: false,
            pulse_paused: false,
        }
    }

    /// Override the inner-dot diameter (pt).
    pub(crate) fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    /// Suppress the **waiting** pulse — pass [`nice_model::Tab::waiting_acknowledged`]
    /// (never recomputed here).
    pub(crate) fn suppress_waiting_pulse(mut self, suppress: bool) -> Self {
        self.suppress_waiting_pulse = suppress;
        self
    }

    /// Freeze the pulse (previews / snapshots).
    pub(crate) fn pulse_paused(mut self, paused: bool) -> Self {
        self.pulse_paused = paused;
        self
    }
}

impl RenderOnce for StatusDot {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let base = status_dot_base_color(self.status, self.idle);
        let base_rgba: Rgba = srgba_to_rgba(base);
        let size = self.size;
        // The outer frame the ring expands within — `size + 4` on a side
        // (`StatusDot.swift:70`). The inner dot centres inside it.
        let frame = size + DOT_FRAME_PADDING;
        let dot_offset = (frame - size) / 2.0;

        // Centre a fixed-size stack; the ring/dot are absolutely positioned so
        // the growing ring overflows outward without disturbing layout.
        let mut root = div().relative().w(px(frame)).h(px(frame));

        let pulses = status_dot_should_pulse(self.status, self.suppress_waiting_pulse)
            && !self.pulse_paused;

        if pulses {
            let (ring, breathe) = pulse_specs(self.status);

            // Expanding outer ring: scale `1.0 → max_scale` (easeOut) while
            // opacity fades `start_opacity → 0`, repeating forever without
            // autoreversing (`StatusDot.swift:91-102`). Painted as an
            // antialiased circle path (see [`paint_pulse_ring`]) rather than an
            // animated `rounded_full` div, so device-pixel snapping can't make
            // the growing ring shake left/right.
            let ring_id: ElementId = (self.id.clone(), "status-ring").into();
            let ring_color = base_rgba;
            let ring_layer = canvas(|_, _, _| (), |_, _, _, _| ()).with_animation(
                ring_id,
                Animation::new(Duration::from_secs_f32(ring.duration_secs))
                    .repeat()
                    .with_easing(ease_out_quadratic),
                move |_canvas, delta| {
                    // Rebuild the canvas each frame bound to this `delta`; the
                    // `frame`-sized absolute box centres the ring on the dot,
                    // and the path overflows it (no clip) as it grows.
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            paint_pulse_ring(bounds, ring_color, delta, ring, window);
                        },
                    )
                    .absolute()
                    .left(px(0.0))
                    .top(px(0.0))
                    .w(px(frame))
                    .h(px(frame))
                },
            );

            // Breathing inner dot: opacity oscillates `min_opacity ↔ 1.0` with an
            // ease-in-out that autoreverses (`StatusDot.swift:104-113`). gpui's
            // `repeat()` is a non-autoreversing sawtooth, so `bounce(ease_in_out)`
            // supplies the there-and-back shape and the period is `2 ×` the
            // Swift half-cycle so each direction keeps its cited duration.
            let breathe_id: ElementId = (self.id.clone(), "status-breathe").into();
            let breathe_layer = div()
                .absolute()
                .left(px(dot_offset))
                .top(px(dot_offset))
                .w(px(size))
                .h(px(size))
                .rounded_full()
                .bg(base_rgba)
                .opacity(breathe.min_opacity)
                .with_animation(
                    breathe_id,
                    Animation::new(Duration::from_secs_f32(2.0 * breathe.duration_secs))
                        .repeat()
                        .with_easing(bounce(ease_in_out)),
                    move |el, eased| {
                        let opacity =
                            breathe.min_opacity + eased * (BREATHE_MAX_OPACITY - breathe.min_opacity);
                        el.opacity(opacity)
                    },
                );

            root = root.child(ring_layer).child(breathe_layer);
        } else {
            // Static filled dot, full opacity (`StatusDot.swift:64-68`).
            root = root.child(
                div()
                    .absolute()
                    .left(px(dot_offset))
                    .top(px(dot_offset))
                    .w(px(size))
                    .h(px(size))
                    .rounded_full()
                    .bg(base_rgba),
            );
        }

        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idle() -> Srgba {
        Srgba::rgb(0.460, 0.441, 0.427) // Nice/Dark ink3.
    }

    #[test]
    fn base_color_maps_status_per_swift() {
        // StatusDot.swift:27-37 — thinking is the FIXED THINKING_DOT brand
        // accent, NOT a caller-supplied swatch.
        assert_eq!(status_dot_base_color(TabStatus::Thinking, idle()), THINKING_DOT);
        assert_eq!(status_dot_base_color(TabStatus::Waiting, idle()), WAITING_DOT);
        assert_eq!(status_dot_base_color(TabStatus::Idle, idle()), idle());
    }

    #[test]
    fn thinking_always_pulses() {
        // StatusDot.swift:48 — thinking pulses regardless of suppression.
        assert!(status_dot_should_pulse(TabStatus::Thinking, false));
        assert!(status_dot_should_pulse(TabStatus::Thinking, true));
    }

    #[test]
    fn waiting_pulses_until_acknowledged() {
        // StatusDot.swift:49 — waiting pulses only while NOT acknowledged.
        assert!(status_dot_should_pulse(TabStatus::Waiting, false));
        assert!(!status_dot_should_pulse(TabStatus::Waiting, true));
    }

    #[test]
    fn idle_never_pulses() {
        // StatusDot.swift:50 — idle never pulses.
        assert!(!status_dot_should_pulse(TabStatus::Idle, false));
        assert!(!status_dot_should_pulse(TabStatus::Idle, true));
    }

    #[test]
    fn pulse_specs_select_per_status() {
        assert_eq!(pulse_specs(TabStatus::Waiting), (WAITING_RING, WAITING_BREATHE));
        assert_eq!(pulse_specs(TabStatus::Thinking), (THINKING_RING, THINKING_BREATHE));
    }

    #[test]
    fn ease_out_quadratic_is_monotone_0_to_1() {
        assert_eq!(ease_out_quadratic(0.0), 0.0);
        assert_eq!(ease_out_quadratic(1.0), 1.0);
        // Eases OUT: past the midpoint by t=0.5 (0.75 > 0.5).
        assert!(ease_out_quadratic(0.5) > 0.5);
    }

    #[test]
    fn suppress_flag_and_helpers_never_borrow_across_status() {
        // The "reads R8, never recomputes" contract in helper form: the pulse
        // decision is a pure function of (status, suppress) only.
        for &status in &[TabStatus::Thinking, TabStatus::Waiting, TabStatus::Idle] {
            let a = status_dot_should_pulse(status, false);
            let b = status_dot_should_pulse(status, true);
            // Only waiting is sensitive to the suppress flag.
            if status == TabStatus::Waiting {
                assert_ne!(a, b);
            } else {
                assert_eq!(a, b);
            }
        }
    }
}
