//! Pure slot-math resolver for reordering pane pills in the horizontal
//! top-bar strip. Ported from `Sources/Nice/Views/PaneStripDropResolver.swift`
//! — **only** the `.slot` (intra-strip reorder) destination. Swift's
//! `PaneDropDestination` also carries `.otherWindowStrip` / `.otherWindowNewTab`
//! / `.newWindow` (the cross-window-move and tear-off cases) plus
//! `PaneDragOrigin.sourceWindowSessionId` / `sourceIndex` — all of that is P5/P6,
//! **CUT** 2026-07-05 (roadmap `notes/rewrite-feature-roadmap-20260702.md`
//! Stage 7 R25). This module ports none of it: [`resolve`] returns only the
//! single slot outcome `(target_pane_id, place_after)`.
//!
//! No `gpui` dependency — pure geometry over [`crate::strip_geometry::Rect`]
//! frames, consumed by the view layer in `crates/nice`.

use std::collections::HashMap;

use crate::strip_geometry::Rect;

/// Pick the pane slot a cursor x-coordinate points at within a horizontal
/// strip: left of the first pill's frame → before it; right of the last
/// pill's frame → after it; over a pill's frame → midpoint split
/// (`place_after = x > mid_x`, a bare split with **no hysteresis / dead-band**
/// — Swift has none, `PaneStripDropResolver.swift:118` `x > frame.midX`).
///
/// Ids without a frame (e.g. scrolled offscreen, no `bounds_for_item`) are
/// simply not drop targets — the guard-on-missing-frame is ported faithfully
/// from Swift's `paneFrames[id]` optional lookups.
///
/// `PaneStripDropResolver.swift:109-128`.
pub fn pane_target(
    x: f32,
    pane_order: &[String],
    frames: &HashMap<String, Rect>,
) -> Option<(String, bool)> {
    if pane_order.is_empty() {
        return None;
    }
    if let Some(first_id) = pane_order.first() {
        if let Some(first_frame) = frames.get(first_id) {
            if x < first_frame.min_x() {
                return Some((first_id.clone(), false));
            }
        }
    }
    if let Some(last_id) = pane_order.last() {
        if let Some(last_frame) = frames.get(last_id) {
            if x > last_frame.max_x() {
                return Some((last_id.clone(), true));
            }
        }
    }
    for id in pane_order {
        let Some(frame) = frames.get(id) else {
            continue;
        };
        if x >= frame.min_x() && x <= frame.max_x() {
            let mid_x = frame.min_x() + frame.width / 2.0;
            return Some((id.clone(), x > mid_x));
        }
    }
    None
}

/// Resolve a drag hovering inside the strip into a reorder slot, gated by the
/// no-op check: a resolved slot that would not actually move the pane (e.g. a
/// self-drop, or dropping adjacent-and-already-there) resolves to `None`
/// rather than a no-op outcome. `would_move` is injected so this stays pure —
/// callers pass a closure wrapping [`crate::TabModel::would_move_pane`], which
/// already closes over the dragged pane id and tab id (the `_dragged_pane_id`
/// parameter here exists for call-site clarity/parity with the Swift
/// `resolve(draggedPaneId:...)` signature; the gate itself is delegated
/// entirely to `would_move`).
///
/// `PaneStripDropResolver.swift:88-103`.
pub fn resolve(
    _dragged_pane_id: &str,
    x: f32,
    pane_order: &[String],
    frames: &HashMap<String, Rect>,
    would_move: impl Fn(&str, bool) -> bool,
) -> Option<(String, bool)> {
    let (target_id, place_after) = pane_target(x, pane_order, frames)?;
    if !would_move(&target_id, place_after) {
        return None;
    }
    Some((target_id, place_after))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    fn frames(pairs: &[(&str, f32, f32)]) -> HashMap<String, Rect> {
        pairs
            .iter()
            .map(|(id, x, width)| (id.to_string(), Rect::new(*x, *width)))
            .collect()
    }

    // -- pane_target ---------------------------------------------------

    #[test]
    fn pane_target_empty_order_is_none() {
        let order: Vec<String> = Vec::new();
        let frames = HashMap::new();
        assert_eq!(pane_target(50.0, &order, &frames), None);
    }

    #[test]
    fn pane_target_left_of_first_frame_is_before_first() {
        let order = order(&["a", "b"]);
        let frames = frames(&[("a", 100.0, 50.0), ("b", 150.0, 50.0)]);
        assert_eq!(
            pane_target(10.0, &order, &frames),
            Some(("a".to_string(), false))
        );
    }

    #[test]
    fn pane_target_right_of_last_frame_is_after_last() {
        let order = order(&["a", "b"]);
        let frames = frames(&[("a", 0.0, 50.0), ("b", 50.0, 50.0)]);
        assert_eq!(
            pane_target(500.0, &order, &frames),
            Some(("b".to_string(), true))
        );
    }

    #[test]
    fn pane_target_below_midpoint_is_before() {
        let order = order(&["a", "b"]);
        // "b" frame is [50, 100), mid_x = 75.
        let frames = frames(&[("a", 0.0, 50.0), ("b", 50.0, 50.0)]);
        assert_eq!(
            pane_target(60.0, &order, &frames),
            Some(("b".to_string(), false))
        );
    }

    #[test]
    fn pane_target_above_midpoint_is_after() {
        let order = order(&["a", "b"]);
        // "b" frame is [50, 100), mid_x = 75.
        let frames = frames(&[("a", 0.0, 50.0), ("b", 50.0, 50.0)]);
        assert_eq!(
            pane_target(90.0, &order, &frames),
            Some(("b".to_string(), true))
        );
    }

    #[test]
    fn pane_target_exactly_at_midpoint_is_before() {
        // Bare `x > mid_x` split — no dead-band. Exactly-at-midpoint fails
        // the `>` and falls to `place_after = false`.
        let order = order(&["a"]);
        let frames = frames(&[("a", 0.0, 100.0)]);
        assert_eq!(
            pane_target(50.0, &order, &frames),
            Some(("a".to_string(), false))
        );
    }

    #[test]
    fn pane_target_inter_pill_gap_is_none() {
        // A gap between two frames that doesn't overlap either, and isn't
        // left-of-first or right-of-last, matches no branch.
        let order = order(&["a", "b"]);
        let frames = frames(&[("a", 0.0, 40.0), ("b", 60.0, 40.0)]);
        assert_eq!(pane_target(50.0, &order, &frames), None);
    }

    #[test]
    fn pane_target_skips_ids_with_no_frame() {
        // "b" is offscreen (no frame). Cursor sits in "b"'s conceptual slot
        // but since it isn't a real frame, it must not be picked up by the
        // first/last checks (only "a" and "c" have frames) or the loop.
        let order = order(&["a", "b", "c"]);
        let frames = frames(&[("a", 0.0, 40.0), ("c", 200.0, 40.0)]);
        // Within "a"'s frame still resolves normally.
        assert_eq!(
            pane_target(10.0, &order, &frames),
            Some(("a".to_string(), false))
        );
        // In the gap where "b" would be (no frame) resolves to None.
        assert_eq!(pane_target(100.0, &order, &frames), None);
    }

    #[test]
    fn pane_target_missing_first_frame_falls_through_to_loop() {
        // First id has no frame, so the left-of-first guard can't fire;
        // falls through to the loop, which finds "b".
        let order = order(&["a", "b"]);
        let frames = frames(&[("b", 0.0, 100.0)]);
        assert_eq!(
            pane_target(10.0, &order, &frames),
            Some(("b".to_string(), false))
        );
    }

    // -- resolve --------------------------------------------------------

    #[test]
    fn resolve_self_drop_no_op_is_none() {
        let order = order(&["a", "b"]);
        let frames = frames(&[("a", 0.0, 50.0), ("b", 50.0, 50.0)]);
        // would_move always false simulates a self-drop / already-there no-op.
        assert_eq!(
            resolve("a", 10.0, &order, &frames, |_, _| false),
            None
        );
    }

    #[test]
    fn resolve_adjacent_no_op_is_none() {
        let order = order(&["a", "b"]);
        let frames = frames(&[("a", 0.0, 50.0), ("b", 50.0, 50.0)]);
        assert_eq!(
            resolve("b", 10.0, &order, &frames, |_, _| false),
            None
        );
    }

    #[test]
    fn resolve_real_move_is_some() {
        let order = order(&["a", "b"]);
        let frames = frames(&[("a", 0.0, 50.0), ("b", 50.0, 50.0)]);
        assert_eq!(
            resolve("a", 10.0, &order, &frames, |_, _| true),
            Some(("a".to_string(), false))
        );
    }

    #[test]
    fn resolve_no_target_is_none_regardless_of_would_move() {
        let order: Vec<String> = Vec::new();
        let frames = HashMap::new();
        assert_eq!(
            resolve("a", 10.0, &order, &frames, |_, _| true),
            None
        );
    }
}
