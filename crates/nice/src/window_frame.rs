//! W6 window-frame persistence math — the pure Cocoa↔gpui conversion pair and
//! the visible-screen clamp, plus the gpui adapter that turns a saved
//! [`crate::session_store::PersistedFrame`] into restore-time
//! [`gpui::WindowOptions`] bounds.
//!
//! ## Coordinate spaces
//!
//! The persisted convention is **Cocoa bottom-left screen points** (origin at
//! the bottom-left of the primary screen, y up) — identical to Swift's
//! `PersistedFrame{x,y,width,height}`, so migration needs no value conversion
//! and [`crate::platform::window_screen_frame`] (already Cocoa) captures it
//! verbatim. gpui's `WindowOptions.window_bounds` wants **top-left** points
//! (origin at the top-left of the global display arrangement, y down), so
//! restore converts once at open. The conversion needs the primary display
//! height (Cocoa's y datum is that screen's bottom).
//!
//! ## Clamp (no Swift math to port — AppKit clamped for free)
//!
//! gpui applies the requested bounds literally, so a rect saved on a
//! now-disconnected external display would open off-screen. The clamp discards a
//! saved rect that intersects **every** display's visible bounds by less than
//! [`MIN_VISIBLE_W`]×[`MIN_VISIBLE_H`] points (default placement then); a rect
//! with a big-enough overlap on some display is used **unchanged** (we never
//! nudge it — a slightly-clipped window is fine, a fully-off-screen one is not).
//!
//! The conversion + clamp are pure functions over plain `f64` rects so they are
//! unit-tested without a gpui `App` (Swift never had these — the tests are
//! Rust-new, per the plan). The `App`-carrying adapter
//! ([`restored_window_bounds`]) is the thin glue.

use gpui::{px, size, App, Bounds, DisplayId, Pixels, Point};

use crate::session_store::PersistedFrame;

/// Minimum on-screen overlap (points) a saved rect must have with some display
/// to be honored — below this on every display, it is discarded for default
/// placement. Chosen so at least the traffic-light row (52pt tall,
/// [`nice_theme::chrome_geometry::TOP_BAR_HEIGHT`]) and a grabbable width remain
/// reachable.
pub const MIN_VISIBLE_W: f64 = 100.0;
pub const MIN_VISIBLE_H: f64 = 52.0;

/// A plain top-left rectangle in logical points — the gpui-space intermediary
/// the pure math operates on (no gpui types, so it is `App`-free testable).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Convert a Cocoa bottom-left frame `[x, y, w, h]` to a top-left [`Rect`], given
/// the primary display's height (Cocoa's y datum). `x`/`w`/`h` pass through; the
/// top-left y is `primary_height − (cocoa_y + h)`.
pub fn cocoa_to_gpui(frame: [f64; 4], primary_height: f64) -> Rect {
    Rect {
        x: frame[0],
        y: primary_height - (frame[1] + frame[3]),
        w: frame[2],
        h: frame[3],
    }
}

/// Inverse of [`cocoa_to_gpui`]: a top-left [`Rect`] back to a Cocoa bottom-left
/// frame `[x, y, w, h]`. The round trip through both is the identity (pinned by
/// [`tests::cocoa_gpui_round_trips`]). Half of the exported Cocoa↔gpui conversion
/// pair (reusable by any future window-placement consumer); production restore
/// only needs the Cocoa→gpui direction so far, hence the allow.
#[allow(dead_code)]
pub fn gpui_to_cocoa(rect: Rect, primary_height: f64) -> [f64; 4] {
    [rect.x, primary_height - (rect.y + rect.h), rect.w, rect.h]
}

/// The width and height of the overlap between two top-left rects (both `0.0`
/// when they are disjoint).
fn overlap(a: &Rect, b: &Rect) -> (f64, f64) {
    let ix0 = a.x.max(b.x);
    let iy0 = a.y.max(b.y);
    let ix1 = (a.x + a.w).min(b.x + b.w);
    let iy1 = (a.y + a.h).min(b.y + b.h);
    ((ix1 - ix0).max(0.0), (iy1 - iy0).max(0.0))
}

/// Keep `rect` iff it overlaps **some** display in `displays` by at least
/// [`MIN_VISIBLE_W`]×[`MIN_VISIBLE_H`] points; otherwise `None` (fall back to
/// default placement). The rect is returned **unchanged** — the clamp accepts or
/// rejects, it does not reposition.
pub fn clamp_to_displays(rect: Rect, displays: &[Rect]) -> Option<Rect> {
    for d in displays {
        let (iw, ih) = overlap(&rect, d);
        if iw >= MIN_VISIBLE_W && ih >= MIN_VISIBLE_H {
            return Some(rect);
        }
    }
    None
}

/// Turn a saved [`PersistedFrame`] (Cocoa points) into restore-time gpui bounds
/// + the id of the display it lands on, or `None` when the frame is missing or
/// clamped away (⇒ default placement). Reads the live display arrangement from
/// `cx`. Not pure — the thin adapter over [`cocoa_to_gpui`] + [`clamp_to_displays`].
pub fn restored_window_bounds(
    frame: Option<&PersistedFrame>,
    cx: &App,
) -> Option<(Bounds<Pixels>, Option<DisplayId>)> {
    let frame = frame?;
    let primary = cx.primary_display()?;
    let primary_height = f64::from(primary.bounds().size.height);
    let rect = cocoa_to_gpui(
        [frame.x, frame.y, frame.width, frame.height],
        primary_height,
    );

    // Each connected display's gpui top-left bounds, as plain rects for the
    // pure clamp.
    let display_rects: Vec<Rect> = cx
        .displays()
        .iter()
        .map(|d| {
            let b = d.bounds();
            Rect {
                x: f64::from(b.origin.x),
                y: f64::from(b.origin.y),
                w: f64::from(b.size.width),
                h: f64::from(b.size.height),
            }
        })
        .collect();

    let kept = clamp_to_displays(rect, &display_rects)?;
    let bounds = Bounds {
        origin: Point {
            x: px(kept.x as f32),
            y: px(kept.y as f32),
        },
        size: size(px(kept.w as f32), px(kept.h as f32)),
    };
    // Open on the display whose bounds contain the rect's top-left origin, else
    // let gpui default (primary).
    let display_id = cx.displays().iter().find_map(|d| {
        let b = d.bounds();
        let contains = kept.x >= f64::from(b.origin.x)
            && kept.x < f64::from(b.origin.x + b.size.width)
            && kept.y >= f64::from(b.origin.y)
            && kept.y < f64::from(b.origin.y + b.size.height);
        contains.then(|| d.id())
    });
    Some((bounds, display_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    // MARK: - Cocoa ↔ gpui conversion (Rust-new; Swift never converted — AppKit
    // restored the Cocoa frame directly).

    #[test]
    fn cocoa_to_gpui_flips_y_about_primary_height() {
        // A 640-tall window whose Cocoa bottom edge sits 100pt up from the
        // primary bottom, on a 1200-tall primary: its top-left y is
        // 1200 - (100 + 640) = 460.
        let rect = cocoa_to_gpui([160.0, 100.0, 960.0, 640.0], 1200.0);
        assert_eq!(rect, Rect { x: 160.0, y: 460.0, w: 960.0, h: 640.0 });
    }

    #[test]
    fn cocoa_gpui_round_trips() {
        let primary = 1440.0;
        let frame = [12.0, 34.0, 800.0, 600.0];
        let back = gpui_to_cocoa(cocoa_to_gpui(frame, primary), primary);
        assert_eq!(back, frame);
    }

    // MARK: - clamp

    fn primary_1440() -> Rect {
        Rect { x: 0.0, y: 0.0, w: 2560.0, h: 1440.0 }
    }

    #[test]
    fn clamp_keeps_a_fully_on_screen_rect_unchanged() {
        let rect = Rect { x: 100.0, y: 100.0, w: 960.0, h: 640.0 };
        assert_eq!(clamp_to_displays(rect, &[primary_1440()]), Some(rect));
    }

    #[test]
    fn clamp_discards_a_fully_off_screen_rect() {
        // Saved on a since-disconnected display to the right of the primary.
        let rect = Rect { x: 4000.0, y: 200.0, w: 960.0, h: 640.0 };
        assert_eq!(clamp_to_displays(rect, &[primary_1440()]), None);
    }

    #[test]
    fn clamp_keeps_a_partially_overlapping_rect_unchanged() {
        // Hangs off the right edge but keeps well over 100×52 on screen.
        let rect = Rect { x: 2400.0, y: 100.0, w: 960.0, h: 640.0 };
        // overlap width = 2560 - 2400 = 160 ≥ 100, height 640 ≥ 52 → kept.
        assert_eq!(clamp_to_displays(rect, &[primary_1440()]), Some(rect));
    }

    #[test]
    fn clamp_discards_a_sliver_below_the_threshold() {
        // Only 40pt of width remains on screen — below MIN_VISIBLE_W.
        let rect = Rect { x: 2520.0, y: 100.0, w: 960.0, h: 640.0 };
        assert_eq!(clamp_to_displays(rect, &[primary_1440()]), None);
    }

    #[test]
    fn clamp_accepts_overlap_on_a_secondary_display() {
        // Off the primary but well inside a second display to its right.
        let secondary = Rect { x: 2560.0, y: 0.0, w: 1920.0, h: 1080.0 };
        let rect = Rect { x: 2700.0, y: 100.0, w: 800.0, h: 600.0 };
        assert_eq!(
            clamp_to_displays(rect, &[primary_1440(), secondary]),
            Some(rect)
        );
    }

    #[test]
    fn clamp_boundary_is_inclusive_at_exactly_the_minimum() {
        // Exactly 100 wide × 52 tall overlap is accepted (>=).
        let rect = Rect { x: 2460.0, y: 1388.0, w: 500.0, h: 500.0 };
        // overlap width = 2560-2460 = 100, height = 1440-1388 = 52.
        assert_eq!(clamp_to_displays(rect, &[primary_1440()]), Some(rect));
    }
}
