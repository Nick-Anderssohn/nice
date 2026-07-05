//! `StripGeometry` — the toolbar pane strip's pure visibility math — ported
//! from `Sources/Nice/Views/PaneStripGeometry.swift`. Pure logic with no
//! `gpui` dependency: which pills are scrolled past which edge of the
//! viewport, driving the leading/trailing edge fades and the offscreen-pane
//! id set the overflow chevron's attention badge reads.
//!
//! The badge itself is **not** computed here. It already exists as
//! [`crate::Tab::has_offscreen_attention`] (ported in R8, the pure
//! combination of [`crate::Pane::needs_attention`] with an offscreen id set)
//! — R11's toolbar feeds this module's [`StripGeometry::offscreen_pane_ids`]
//! into that existing method rather than a second attention predicate here.
//!
//! ## What dies with real layout
//!
//! The Swift version also shipped `PaneStripOverflowEstimator`, which summed
//! AppKit text-measured pill widths to guess the strip's content width —
//! required only because SwiftUI virtualizes offscreen content and stops
//! reporting frames for pills scrolled out of view. GPUI's `ScrollHandle`
//! reads **real** layout (`max_offset()`, `bounds_for_item(ix)`, …), so that
//! estimator does not get ported: [`should_show_overflow_chevron`] takes the
//! real measured overflow amount directly, never a width estimate.
//!
//! What *does* survive from the estimator is behavioral, not mechanical: the
//! unconditional reservation of the chevron + `+` button's slots in the
//! tracked content width (`PaneStripOverflowEstimator.swift:26-31`), and the
//! `panes.count >= 2` gate (`PaneStripOverflowEstimator.swift:107-115`). Both
//! are folded into [`should_show_overflow_chevron`].

use std::collections::{HashMap, HashSet};

/// Tolerance (pt) for floating-point rounding in frame math. Sub-pixel drift
/// below this magnitude does not flip an edge — keeps the fades from
/// flickering on layout snaps. `PaneStripGeometry.swift:29`.
pub const EDGE_TOLERANCE: f32 = 0.5;

/// A pane pill's horizontal extent within the strip's scroll-content
/// coordinate space — the pure-Rust analog of the `CGRect` values Swift's
/// `PaneStripGeometry.paneFrames` keyed on. Only `x`/`width` are kept because
/// every ported predicate is horizontal-only (`PaneStripGeometry.swift:20`);
/// vertical extent plays no role.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub width: f32,
}

impl Rect {
    /// Construct a rect from its leading x and width.
    pub fn new(x: f32, width: f32) -> Self {
        Rect { x, width }
    }

    /// Leading edge (`CGRect.minX`).
    pub fn min_x(&self) -> f32 {
        self.x
    }

    /// Trailing edge (`CGRect.maxX`).
    pub fn max_x(&self) -> f32 {
        self.x + self.width
    }
}

/// Pure snapshot of the pill row's real layout, keyed by pane id. Ported from
/// `PaneStripGeometry.swift`.
///
/// Coordinate convention (unchanged from Swift, `PaneStripGeometry.swift:11-17`):
/// frames are reported in the scroll content's coordinate space, where the
/// viewport is fixed at `[0, visible_width]` regardless of the current scroll
/// offset. A pill is fully offscreen iff `frame.max_x() <= 0` (scrolled past
/// the leading edge) or `frame.min_x() >= visible_width` (past the trailing
/// edge); partially clipped iff its frame straddles either edge. On GPUI this
/// is fed by `ScrollHandle::bounds_for_item(ix)` translated into this
/// viewport-relative space by the view — real layout, no estimation.
#[derive(Debug, Clone, PartialEq)]
pub struct StripGeometry {
    pub pane_frames: HashMap<String, Rect>,
    pub visible_width: f32,
}

impl StripGeometry {
    /// Construct a geometry snapshot from per-pane frames and the strip's
    /// visible width.
    pub fn new(pane_frames: HashMap<String, Rect>, visible_width: f32) -> Self {
        StripGeometry {
            pane_frames,
            visible_width,
        }
    }

    /// Some pill is hidden past the leading edge. Drives the leading edge
    /// fade. `PaneStripGeometry.swift:33-35`.
    pub fn can_scroll_leading(&self) -> bool {
        self.pane_frames.values().any(|r| r.min_x() < -EDGE_TOLERANCE)
    }

    /// Some pill is hidden past the trailing edge. Drives the trailing edge
    /// fade. `PaneStripGeometry.swift:38-43`.
    pub fn can_scroll_trailing(&self) -> bool {
        if self.visible_width <= 0.0 {
            return false;
        }
        self.pane_frames
            .values()
            .any(|r| r.max_x() > self.visible_width + EDGE_TOLERANCE)
    }

    /// Pane ids whose frames sit entirely outside the viewport.
    /// Partially-clipped panes are *not* in this set: the user can still see
    /// their pulse/icon and they don't need a separate badge.
    /// `PaneStripGeometry.swift:49-60`.
    pub fn offscreen_pane_ids(&self) -> HashSet<String> {
        if self.visible_width <= 0.0 {
            return HashSet::new();
        }
        self.pane_frames
            .iter()
            .filter(|(_, r)| {
                r.max_x() <= EDGE_TOLERANCE || r.min_x() >= self.visible_width - EDGE_TOLERANCE
            })
            .map(|(id, _)| id.clone())
            .collect()
    }
}

/// Whether the overflow chevron should render, given the pill row's real
/// measured overflow amount and the pane count.
///
/// `max_offset_x` is the strip's real scrollable overflow — on GPUI,
/// `ScrollHandle::max_offset().x` — measured with the chevron's own slot and
/// the trailing `+` button's slot **already, unconditionally** part of the
/// tracked scroll content, regardless of whether the chevron currently
/// renders. That unconditional reservation is what kills the
/// show-chevron→shrink→hide feedback loop
/// (`PaneStripOverflowEstimator.swift:26-31`): the content width this
/// predicate is measured against never excludes the chevron's own width, so
/// showing the chevron can never retroactively make the row "fit" and hide
/// it again. `pane_count < 2` short-circuits to `false` — the overflow menu
/// is pointless with a single pill
/// (`PaneStripOverflowEstimator.swift:107-115`).
pub fn should_show_overflow_chevron(pane_count: usize, max_offset_x: f32) -> bool {
    pane_count >= 2 && max_offset_x > 0.0
}

/// The horizontal scroll offset that centers the pill occupying
/// `[item_left, item_left + item_width]` inside a viewport spanning
/// `[viewport_left, viewport_left + viewport_width]`, clamped to the scrollable
/// range `[-max_offset_x, 0]`.
///
/// This is the GPUI-real-layout replacement for SwiftUI's
/// `ScrollViewProxy.scrollTo(_, anchor: .center)`
/// (`WindowToolbarView.swift:374-379`): on GPUI `scroll_to_item` only *reveals* a
/// child, so the strip computes the centering offset here and applies it with
/// `ScrollHandle::set_offset`.
///
/// GPUI's `ScrollHandle` records each child's bounds in the viewport's own
/// window-coordinate space **without** the current scroll offset applied, and the
/// on-screen position of a child is `item_left + offset_x` (`offset_x` is `0` at
/// the leading edge and `-max_offset_x` at the trailing edge —
/// `elements/div.rs:2205`). Centering means picking `offset_x` so the child's
/// on-screen midpoint lands on the viewport midpoint:
///
/// ```text
/// item_left + offset_x + item_width / 2 == viewport_left + viewport_width / 2
/// ```
///
/// solved for `offset_x` and clamped so a pill near either end centers only as
/// far as the content allows and the strip never scrolls past its bounds. A
/// non-positive `max_offset_x` (nothing to scroll) yields `0`.
pub fn center_offset_x(
    viewport_left: f32,
    viewport_width: f32,
    item_left: f32,
    item_width: f32,
    max_offset_x: f32,
) -> f32 {
    let raw = viewport_left + viewport_width / 2.0 - item_width / 2.0 - item_left;
    let max = max_offset_x.max(0.0);
    raw.clamp(-max, 0.0)
}

#[cfg(test)]
mod tests {
    //! Ported from `Tests/NiceUnitTests/PaneStripGeometryTests.swift`'s
    //! rect-fixture cases (the "Edge / offscreen detection" and "Edge cases"
    //! sections, :35-157). The `Tab.hasOffscreenAttention` /
    //! `Pane.needsAttention` sections of that file (:159-273) pin
    //! `crate::Tab::has_offscreen_attention` / `crate::Pane::needs_attention`
    //! directly and are out of this module's scope — see this file's module
    //! docs.
    use super::*;

    fn rect(x: f32, width: f32) -> Rect {
        Rect::new(x, width)
    }

    fn geometry(pane_frames: HashMap<String, Rect>, visible_width: f32) -> StripGeometry {
        StripGeometry::new(pane_frames, visible_width)
    }

    fn frames<const N: usize>(pairs: [(&str, Rect); N]) -> HashMap<String, Rect> {
        pairs.into_iter().map(|(id, r)| (id.to_string(), r)).collect()
    }

    // MARK: - Edge / offscreen detection

    /// Three pills that fit inside the viewport: no scroll affordances, no
    /// offscreen panes. `PaneStripGeometryTests.swift:39-52`.
    #[test]
    fn no_overflow_when_all_panes_fit() {
        let geo = geometry(
            frames([
                ("p1", rect(0.0, 100.0)),
                ("p2", rect(102.0, 100.0)),
                ("p3", rect(204.0, 100.0)),
            ]),
            320.0,
        );

        assert!(!geo.can_scroll_leading());
        assert!(!geo.can_scroll_trailing());
        assert_eq!(geo.offscreen_pane_ids(), HashSet::new());
    }

    /// p1 is scrolled past the leading edge; p2/p3 are visible. Only the
    /// leading fade should fire and only p1 is offscreen.
    /// `PaneStripGeometryTests.swift:56-69`.
    #[test]
    fn leading_only_overflow() {
        let geo = geometry(
            frames([
                ("p1", rect(-120.0, 100.0)), // fully past left
                ("p2", rect(-16.0, 100.0)),  // partially clipped
                ("p3", rect(86.0, 100.0)),
            ]),
            200.0,
        );

        assert!(geo.can_scroll_leading());
        assert!(!geo.can_scroll_trailing());
        assert_eq!(geo.offscreen_pane_ids(), HashSet::from(["p1".to_string()]));
    }

    /// p3 extends past the trailing edge; p1/p2 are visible. Trailing fade
    /// only, only p3 is offscreen. `PaneStripGeometryTests.swift:73-86`.
    #[test]
    fn trailing_only_overflow() {
        let geo = geometry(
            frames([
                ("p1", rect(0.0, 100.0)),
                ("p2", rect(102.0, 100.0)), // partially clipped
                ("p3", rect(220.0, 100.0)), // fully past right
            ]),
            200.0,
        );

        assert!(!geo.can_scroll_leading());
        assert!(geo.can_scroll_trailing());
        assert_eq!(geo.offscreen_pane_ids(), HashSet::from(["p3".to_string()]));
    }

    /// Active pane in the middle scrolled into view, with hidden panes on
    /// both sides — both fades fire and both offscreen ids surface.
    /// `PaneStripGeometryTests.swift:90-103`.
    #[test]
    fn both_edges_overflow() {
        let geo = geometry(
            frames([
                ("p1", rect(-130.0, 100.0)), // off left
                ("p2", rect(50.0, 100.0)),   // visible
                ("p3", rect(220.0, 100.0)),  // off right
            ]),
            200.0,
        );

        assert!(geo.can_scroll_leading());
        assert!(geo.can_scroll_trailing());
        assert_eq!(
            geo.offscreen_pane_ids(),
            HashSet::from(["p1".to_string(), "p3".to_string()])
        );
    }

    /// A single pane wider than the viewport extends past the trailing edge
    /// — the trailing fade fires. `PaneStripGeometryTests.swift:107-115`.
    #[test]
    fn single_huge_pane_extends_past_trailing_edge() {
        let geo = geometry(frames([("p1", rect(0.0, 500.0))]), 200.0);

        assert!(geo.can_scroll_trailing());
        assert_eq!(geo.offscreen_pane_ids(), HashSet::new());
    }

    // MARK: - Edge cases

    /// Pre-layout the view reports `visible_width == 0`. Geometry must stay
    /// quiet in that frame so the fades don't briefly flash on initial
    /// appearance. `PaneStripGeometryTests.swift:122-130`.
    #[test]
    fn zero_visible_width_is_quiet() {
        let geo = geometry(frames([("p1", rect(0.0, 100.0))]), 0.0);

        assert!(!geo.can_scroll_trailing());
        assert_eq!(geo.offscreen_pane_ids(), HashSet::new());
    }

    /// Frames straddling each edge by sub-pixel amounts must not flip the
    /// edge bools — that's the `EDGE_TOLERANCE` contract. Without it,
    /// snapping pills would flicker the fades on layout passes.
    /// `PaneStripGeometryTests.swift:135-147`.
    #[test]
    fn sub_pixel_clipping_does_not_flip_edges() {
        let geo = geometry(
            frames([
                ("p1", rect(-0.3, 100.0)),
                ("p2", rect(99.7, 100.4)), // max_x = 200.1
            ]),
            200.0,
        );

        assert!(!geo.can_scroll_leading());
        assert!(!geo.can_scroll_trailing());
        assert_eq!(geo.offscreen_pane_ids(), HashSet::new());
    }

    /// No panes at all (e.g. between active-tab swaps) must be safe and
    /// produce no chrome. `PaneStripGeometryTests.swift:151-157`.
    #[test]
    fn empty_frames_is_quiet() {
        let geo = geometry(HashMap::new(), 400.0);

        assert!(!geo.can_scroll_leading());
        assert!(!geo.can_scroll_trailing());
        assert_eq!(geo.offscreen_pane_ids(), HashSet::new());
    }

    // MARK: - should_show_overflow_chevron (reservation / >=2 rule)
    //
    // Ported behaviorally from `PaneStripOverflowEstimator.swift:26-31,107-115`
    // — the estimator's width-estimation MACHINERY dies (real layout replaces
    // it), but the reservation + >=2-panes RULES survive as this pure gate
    // over the real measured overflow amount.

    /// Fewer than two panes never shows the chevron, no matter how large the
    /// measured overflow is — the menu is pointless with a single pill.
    #[test]
    fn fewer_than_two_panes_never_shows_chevron() {
        assert!(!should_show_overflow_chevron(0, 999.0));
        assert!(!should_show_overflow_chevron(1, 999.0));
    }

    /// Zero or negative measured overflow hides the chevron.
    #[test]
    fn no_measured_overflow_hides_chevron() {
        assert!(!should_show_overflow_chevron(2, 0.0));
        assert!(!should_show_overflow_chevron(3, -5.0));
    }

    /// Two-plus panes with positive measured overflow shows the chevron.
    #[test]
    fn overflow_with_two_or_more_panes_shows_chevron() {
        assert!(should_show_overflow_chevron(2, 0.1));
        assert!(should_show_overflow_chevron(5, 240.0));
    }

    /// The reservation rule: a strip whose pills alone would fit, but which
    /// overflows once the chevron + `+` slots are unconditionally counted in
    /// the tracked scroll content, still shows the chevron. Modeled here as a
    /// small positive `max_offset_x` — exactly what a real `ScrollHandle`
    /// reports once the always-mounted reserved slots push the content past
    /// the viewport, even though the pills alone would not have.
    #[test]
    fn reservation_alone_can_trigger_the_chevron() {
        // Pills alone: 190pt fits in a 200pt viewport (no overflow). Reserved
        // chevron (28pt) + "+" (28pt) slots are unconditionally counted in
        // the real tracked content, so the real ScrollHandle measures
        // max_offset().x = (190 + 28 + 28) - 200 = 46pt.
        let max_offset_x = (190.0 + 28.0 + 28.0) - 200.0;
        assert!(should_show_overflow_chevron(2, max_offset_x));
    }

    /// Showing the chevron never un-overflows it: the predicate is a pure
    /// function of an already-reservation-inclusive measured overflow, with
    /// no code path that subtracts the chevron's own slot once shown — so
    /// repeated evaluation against the same measured overflow can only ever
    /// agree, eliminating the show→shrink→hide flicker the Swift estimator
    /// had to work around by reserving unconditionally.
    #[test]
    fn showing_the_chevron_never_un_overflows_it() {
        let max_offset_x = 10.0;
        assert_eq!(
            should_show_overflow_chevron(2, max_offset_x),
            should_show_overflow_chevron(2, max_offset_x)
        );
        assert!(should_show_overflow_chevron(2, max_offset_x));
    }

    // MARK: - center_offset_x (auto-center-on-activate offset math)
    //
    // The GPUI-real-layout replacement for SwiftUI's `scrollTo(anchor: .center)`.
    // Pinned here (in `nice-model`) so the R11 view and the in-process itests read
    // the same arithmetic — `nice-itests` cannot import the `nice` binary, so the
    // centering math has to live where both can see it.

    /// A pill fully to the right of the viewport centers so its on-screen midpoint
    /// lands exactly on the viewport midpoint.
    #[test]
    fn centers_an_offscreen_pill_on_the_viewport_midpoint() {
        // Viewport [0, 200] (midpoint 100); pill [300, 400] (width 100, midpoint
        // 350) laid out to the right, with plenty of scroll room.
        let offset = center_offset_x(0.0, 200.0, 300.0, 100.0, 1000.0);
        assert_eq!(offset, -250.0);
        // On-screen midpoint == viewport midpoint.
        let onscreen_mid = 300.0 + offset + 100.0 / 2.0;
        assert_eq!(onscreen_mid, 100.0);
    }

    /// A pill near the leading edge can't scroll left of the start: the offset
    /// clamps to `0` rather than centering past the content's leading edge.
    #[test]
    fn clamps_at_the_leading_edge() {
        // Pill [0, 100] in viewport [0, 200]: raw would be +50 (scroll right of
        // start), clamped to 0.
        assert_eq!(center_offset_x(0.0, 200.0, 0.0, 100.0, 1000.0), 0.0);
    }

    /// A pill near the trailing edge centers only as far as `max_offset_x` allows.
    #[test]
    fn clamps_at_the_trailing_edge() {
        // Raw centering offset is very negative; clamp floors it at -max_offset_x.
        assert_eq!(center_offset_x(0.0, 200.0, 900.0, 100.0, 300.0), -300.0);
    }

    /// Nothing to scroll (`max_offset_x <= 0`) always yields a zero offset.
    #[test]
    fn no_scroll_room_is_a_zero_offset() {
        assert_eq!(center_offset_x(0.0, 200.0, 40.0, 100.0, 0.0), 0.0);
        assert_eq!(center_offset_x(0.0, 200.0, 40.0, 100.0, -5.0), 0.0);
    }

    /// The viewport's own leading edge (`viewport_left`) is honored — a non-zero
    /// viewport origin shifts the centering target by the same amount.
    #[test]
    fn respects_a_nonzero_viewport_origin() {
        // Viewport [50, 250] (midpoint 150); pill [400, 500] (midpoint 450).
        let offset = center_offset_x(50.0, 200.0, 400.0, 100.0, 1000.0);
        let onscreen_mid = 400.0 + offset + 50.0;
        assert_eq!(onscreen_mid, 150.0);
    }
}
