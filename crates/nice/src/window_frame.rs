//! W6 window-frame persistence math — the pure global-Cocoa→display-relative
//! conversion + the visible-screen clamp, plus the gpui adapter that turns a
//! saved [`crate::session_store::PersistedFrame`] into restore-time
//! [`gpui::WindowOptions`] bounds.
//!
//! ## Coordinate spaces
//!
//! The persisted convention is **global Cocoa bottom-left screen points**
//! (origin at the bottom-left of the primary screen, y up, every other screen
//! placed around it) — identical to Swift's `PersistedFrame{x,y,width,height}`,
//! so migration needs no value conversion and
//! [`crate::platform::window_screen_frame`] (which reads `-[NSWindow frame]`,
//! already global Cocoa) captures it verbatim.
//!
//! gpui's `WindowOptions.window_bounds` is **display-relative top-left points**:
//! the origin is the top-left corner of the display named by
//! `WindowOptions.display_id`, y down. That is not obvious from the type — a
//! `Bounds<Pixels>` looks global — but it is gpui's deliberate convention, not a
//! wart to patch around: `MacWindow::open` places the window at
//!
//! ```text
//! setFrameTopLeftPoint((screen.frame.x + bounds.x,
//!                       screen.frame.y + screen.height - bounds.y))
//! ```
//!
//! its reader `MacWindow::bounds()` inverts exactly that, and
//! `PlatformDisplay::bounds()`/`visible_bounds()` report every macOS display
//! with a zeroed origin for the same reason — each display is its own space.
//!
//! Nice is the side that mixes spaces: it saves a GLOBAL frame (its own
//! `-[NSWindow frame]` read). So restore has to pick the target display itself
//! and express the saved rect relative to it ([`restore_placement`]). Feeding
//! gpui global x (the pre-fix behavior) reopened windows shifted right by the
//! target display's Cocoa origin — invisible on a single-display Mac (origin 0),
//! badly off-screen on a multi-display one.
//!
//! The saved format stays global Cocoa on purpose: persisting display-relative
//! bounds + a display id instead (what zed itself does) would be equally
//! correct, but it changes the on-disk shape and would restore every existing
//! `sessions.json` wrong. Converting at restore needs no migration.
//!
//! ## Clamp (no Swift math to port — AppKit clamped for free)
//!
//! gpui applies the requested bounds literally, so a rect saved on a
//! now-disconnected external display would open off-screen. The clamp discards a
//! saved rect that overlaps **every** screen by less than
//! [`MIN_VISIBLE_W`]×[`MIN_VISIBLE_H`] points (default placement then); among the
//! screens it does overlap enough, the one it overlaps by the largest **area** is
//! the display it reopens on — the same "mostly on this screen" rule AppKit uses
//! to associate a window with a screen. The rect is used **unchanged** (we never
//! nudge it — a slightly-clipped window is fine, a fully-off-screen one is not).
//!
//! The screen pick + conversion are pure functions over plain `f64` rects, so
//! they are unit-tested without a gpui `App` or a real display arrangement
//! (Swift never had these — the tests are Rust-new, per the plan). The adapter
//! ([`restored_window_bounds`]) is the thin glue over
//! [`crate::platform::screens`].

use gpui::{px, size, Bounds, DisplayId, Pixels, Point};

use crate::session_store::PersistedFrame;

/// Minimum on-screen overlap (points) a saved rect must have with some display
/// to be honored — below this on every display, it is discarded for default
/// placement. Chosen so at least the traffic-light row (52pt tall,
/// [`nice_theme::chrome_geometry::TOP_BAR_HEIGHT`]) and a grabbable width remain
/// reachable.
pub const MIN_VISIBLE_W: f64 = 100.0;
pub const MIN_VISIBLE_H: f64 = 52.0;

/// A plain top-left rectangle in logical points — the gpui-space intermediary
/// the pure math produces (no gpui types, so it is `App`-free testable).
/// **Display-relative**: `x`/`y` are measured from the target display's top-left
/// corner, matching `WindowOptions.window_bounds` (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// One connected screen as the restore math needs it: its `CGDirectDisplayID`
/// (what [`gpui::DisplayId`] wraps on macOS) and its `-[NSScreen frame]` in
/// **global Cocoa points** `[x, y, w, h]` — the space saved frames live in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen {
    pub id: u32,
    pub frame: [f64; 4],
}

/// Where a saved frame reopens: the display to open it on, and the
/// display-relative top-left [`Rect`] gpui wants for it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub display: u32,
    pub bounds: Rect,
}

/// The width and height of the overlap between two rects given as `[x, y, w, h]`
/// (both `0.0` when they are disjoint). Axis-agnostic — used here on global
/// Cocoa rects, where both operands share the same bottom-left space.
fn overlap(a: [f64; 4], b: [f64; 4]) -> (f64, f64) {
    let ix0 = a[0].max(b[0]);
    let iy0 = a[1].max(b[1]);
    let ix1 = (a[0] + a[2]).min(b[0] + b[2]);
    let iy1 = (a[1] + a[3]).min(b[1] + b[3]);
    ((ix1 - ix0).max(0.0), (iy1 - iy0).max(0.0))
}

/// Turn a saved global-Cocoa frame `[x, y, w, h]` into the display +
/// display-relative top-left bounds to reopen it at, or `None` when it overlaps
/// every connected screen by less than [`MIN_VISIBLE_W`]×[`MIN_VISIBLE_H`]
/// (⇒ default placement — the saved display is gone, or the arrangement moved).
///
/// The chosen screen is the one with the largest overlap **area** among those
/// clearing the minimum. The conversion onto it (`[sx, sy, sw, sh]`) is:
///
/// ```text
/// bounds.x = x - sx                  // right of the screen's left edge
/// bounds.y = (sy + sh) - (y + h)     // below the screen's top edge, y down
/// ```
pub fn restore_placement(saved: [f64; 4], screens: &[Screen]) -> Option<Placement> {
    let best = screens
        .iter()
        .filter_map(|s| {
            let (iw, ih) = overlap(saved, s.frame);
            (iw >= MIN_VISIBLE_W && ih >= MIN_VISIBLE_H).then_some((s, iw * ih))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(s, _)| s)?;

    let [x, y, w, h] = saved;
    let [sx, sy, _sw, sh] = best.frame;
    Some(Placement {
        display: best.id,
        bounds: Rect {
            x: x - sx,
            y: (sy + sh) - (y + h),
            w,
            h,
        },
    })
}

/// The inverse of [`restore_placement`]'s conversion: the global Cocoa frame
/// `[x, y, w, h]` a window opened with `bounds` on the screen whose global Cocoa
/// frame is `screen` actually lands at.
///
/// This models gpui + AppKit's placement (`MacWindow::open`'s
/// `setFrameTopLeftPoint`), so a `restore_placement` → `placed_cocoa_frame`
/// round trip recovering the saved frame is the real round-trip assertion
/// ([`tests::placement_round_trips_on_every_screen`]). Test-only — production
/// restore needs the forward direction only.
#[cfg(test)]
fn placed_cocoa_frame(bounds: Rect, screen: [f64; 4]) -> [f64; 4] {
    let [sx, sy, _sw, sh] = screen;
    let top_left_y = sy + sh - bounds.y;
    [sx + bounds.x, top_left_y - bounds.h, bounds.w, bounds.h]
}

/// Turn a saved [`PersistedFrame`] (global Cocoa points) into restore-time gpui
/// bounds + the id of the display to open it on, or `None` when the frame is
/// missing or clamped away (⇒ default placement). Reads the live screen
/// arrangement from [`crate::platform::screens`] rather than `App::displays()`,
/// whose macOS bounds are display-local by design (see the module docs) and so
/// cannot say where a display sits in the global arrangement. The thin adapter
/// over [`restore_placement`].
pub fn restored_window_bounds(
    frame: Option<&PersistedFrame>,
) -> Option<(Bounds<Pixels>, Option<DisplayId>)> {
    let frame = frame?;
    let placement = restore_placement(
        [frame.x, frame.y, frame.width, frame.height],
        &crate::platform::screens(),
    )?;
    let bounds = Bounds {
        origin: Point {
            x: px(placement.bounds.x as f32),
            y: px(placement.bounds.y as f32),
        },
        size: size(px(placement.bounds.w as f32), px(placement.bounds.h as f32)),
    };
    Some((bounds, Some(DisplayId::new(u64::from(placement.display)))))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The arrangement every multi-display case below uses is Nick's machine, the
    // one the bug was found on (`reference_1x_display_rendering_gotchas`): the
    // built-in 2x laptop as the primary (Cocoa origin (0,0), 1470×956 points) and
    // a 1x Samsung C34J79x ultrawide to its right, tops aligned — so the
    // ultrawide's Cocoa origin is (1470, 956 − 1440) = (1470, −484).
    const LAPTOP: Screen = Screen {
        id: 1,
        frame: [0.0, 0.0, 1470.0, 956.0],
    };
    const ULTRAWIDE: Screen = Screen {
        id: 2,
        frame: [1470.0, -484.0, 3440.0, 1440.0],
    };

    fn both() -> Vec<Screen> {
        vec![LAPTOP, ULTRAWIDE]
    }

    // MARK: - display-relative conversion

    #[test]
    fn a_primary_frame_converts_against_the_primary_screen() {
        // Cocoa y 100 + height 629 on a 956-tall primary ⇒ top-left y
        // 956 − 729 = 227; x passes through because the primary's origin is 0.
        let p = restore_placement([200.0, 100.0, 960.0, 629.0], &both()).unwrap();
        assert_eq!(p.display, LAPTOP.id);
        assert_eq!(
            p.bounds,
            Rect { x: 200.0, y: 227.0, w: 960.0, h: 629.0 }
        );
    }

    #[test]
    fn a_secondary_frame_is_relative_to_that_screens_origin() {
        // THE BUG: a window on the ultrawide used to be handed its GLOBAL x, so
        // gpui reopened it at 1470 + x (3291 → 4761, observed). Relative x is
        // x − 1470. y: the ultrawide's top is −484 + 1440 = 956 and the window's
        // top is −20 + 629 = 609, so it sits 347 below that top edge.
        let p = restore_placement([3291.0, -20.0, 960.0, 629.0], &both()).unwrap();
        assert_eq!(p.display, ULTRAWIDE.id);
        assert_eq!(
            p.bounds,
            Rect { x: 1821.0, y: 347.0, w: 960.0, h: 629.0 }
        );
    }

    #[test]
    fn a_frame_left_of_the_secondary_origin_is_reachable() {
        // The pre-fix math could never place a window in the global x range
        // [1470, 2940) on the ultrawide — it added 1470 to whatever it was given.
        // Global x 1821 is 351 points in from the ultrawide's left edge.
        let p = restore_placement([1821.0, 100.0, 960.0, 629.0], &both()).unwrap();
        assert_eq!(p.display, ULTRAWIDE.id);
        assert_eq!(p.bounds.x, 351.0);
    }

    #[test]
    fn placement_round_trips_on_every_screen() {
        // The property the bug violated: reopening a saved frame must land it
        // back at the same GLOBAL Cocoa rect, on either display, including a
        // negative Cocoa y (window bottom below the primary screen's bottom).
        for frame in [
            [200.0, 100.0, 960.0, 629.0],   // laptop
            [3291.0, -20.0, 960.0, 629.0],  // ultrawide, y < 0
            [1500.0, -400.0, 800.0, 600.0], // ultrawide, deep below the datum
            [0.0, 0.0, 1470.0, 956.0],      // laptop, exactly full-screen
        ] {
            let p = restore_placement(frame, &both()).unwrap();
            let screen = both().iter().find(|s| s.id == p.display).unwrap().frame;
            assert_eq!(placed_cocoa_frame(p.bounds, screen), frame, "{frame:?}");
        }
    }

    #[test]
    fn a_single_display_arrangement_is_unaffected() {
        // The arrangement that masked the bug: one screen at the origin, so
        // display-relative == global on both axes.
        let p = restore_placement([160.0, 160.0, 960.0, 640.0], &[LAPTOP]).unwrap();
        assert_eq!(p.display, LAPTOP.id);
        assert_eq!(
            p.bounds,
            Rect { x: 160.0, y: 156.0, w: 960.0, h: 640.0 }
        );
    }

    // MARK: - screen pick

    #[test]
    fn a_straddling_frame_opens_on_the_screen_it_mostly_covers() {
        // Spans the seam at x = 1470: 300 points wide on the laptop, 660 on the
        // ultrawide (both overlaps 629 tall) ⇒ the ultrawide wins, and the
        // relative x is negative because the window starts left of its edge.
        let p = restore_placement([1170.0, 0.0, 960.0, 629.0], &both()).unwrap();
        assert_eq!(p.display, ULTRAWIDE.id);
        assert_eq!(p.bounds.x, -300.0);
    }

    #[test]
    fn a_screen_below_the_minimum_overlap_never_wins() {
        // 60 points onto the laptop (< MIN_VISIBLE_W) but 900 onto the ultrawide:
        // the laptop is not even a candidate, so the sliver cannot capture it.
        let p = restore_placement([1410.0, 0.0, 960.0, 629.0], &both()).unwrap();
        assert_eq!(p.display, ULTRAWIDE.id);
    }

    // MARK: - clamp

    #[test]
    fn a_fully_off_screen_frame_falls_back_to_default_placement() {
        // Saved on a since-disconnected display right of the ultrawide.
        assert_eq!(
            restore_placement([6000.0, 200.0, 960.0, 640.0], &both()),
            None
        );
    }

    #[test]
    fn a_frame_saved_on_a_now_gone_display_falls_back() {
        // The same ultrawide frame, with only the laptop attached.
        assert_eq!(
            restore_placement([3291.0, -20.0, 960.0, 629.0], &[LAPTOP]),
            None
        );
    }

    #[test]
    fn a_partially_overlapping_frame_is_kept_unchanged() {
        // Hangs off the ultrawide's right edge (4910) but keeps 800 points on it.
        let p = restore_placement([4110.0, 0.0, 960.0, 629.0], &both()).unwrap();
        assert_eq!(p.display, ULTRAWIDE.id);
        assert_eq!(p.bounds.w, 960.0);
        assert_eq!(p.bounds.x, 2640.0);
    }

    #[test]
    fn a_sliver_below_the_threshold_is_discarded() {
        // Only 40 points of width remain on the ultrawide (right edge 4910).
        assert_eq!(
            restore_placement([4870.0, 0.0, 960.0, 629.0], &both()),
            None
        );
    }

    #[test]
    fn the_minimum_overlap_is_inclusive() {
        // Exactly 100 wide × 52 tall onto the laptop, and nothing on the
        // ultrawide (the frame's right edge stops at the laptop's, 1470).
        let p = restore_placement([1370.0, 904.0, 100.0, 52.0], &both()).unwrap();
        assert_eq!(p.display, LAPTOP.id);
    }

    #[test]
    fn no_screens_at_all_falls_back() {
        assert_eq!(restore_placement([200.0, 100.0, 960.0, 629.0], &[]), None);
    }
}
