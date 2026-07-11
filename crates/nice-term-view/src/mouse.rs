//! The R5 mouse edge: pure helpers that translate the core's tracked
//! [`TermMode`] mouse bits + a pixel position into what `nice-term-input`'s
//! [`encode_mouse`](nice_term_input::encode_mouse) needs, plus the pixel→cell
//! hit-test reusing R4's grid metrics.
//!
//! Everything here is a pure function; the gpui event glue (the `on_mouse_*`
//! listeners, the local drag-selection state, and the mouse-report writes) lives
//! in [`crate::view`]. Splitting the decisions out keeps them unit-testable
//! without a running window.
//!
//! ## The mode bits
//!
//! alacritty's `TermMode` tracks the DECSET mouse modes the application enabled:
//! `MOUSE_REPORT_CLICK` (1000, press/release only), `MOUSE_DRAG` (1002, adds
//! motion while a button is held), `MOUSE_MOTION` (1003, all motion), and the
//! encoding selectors `SGR_MOUSE` (1006) / `UTF8_MOUSE` (1005). The byte-level
//! encoding is [`nice_term_input`]'s job; this module only reads the bits and
//! picks the [`MouseProtocol`] + reporting decisions.

use alacritty_terminal::term::TermMode;

use nice_term_input::{Modifiers as VtModifiers, MouseButton as VtButton, MouseProtocol};

/// Whether the application has requested any mouse reporting (any of the three
/// tracking modes). When true, a bare (unmodified) click/drag/wheel is reported
/// to the pty rather than driving a local selection; the terminal's local
/// override is a held modifier — see [`crate::view`]'s selection rule.
pub fn reporting_active(mode: TermMode) -> bool {
    mode.intersects(TermMode::MOUSE_MODE)
}

/// The byte-level protocol the app selected. Only meaningful when
/// [`reporting_active`] — SGR (1006) wins over UTF-8 (1005); otherwise the
/// original X10 byte encoding.
pub fn protocol(mode: TermMode) -> MouseProtocol {
    if mode.contains(TermMode::SGR_MOUSE) {
        MouseProtocol::Sgr
    } else if mode.contains(TermMode::UTF8_MOUSE) {
        MouseProtocol::Utf8
    } else {
        MouseProtocol::X10
    }
}

/// Whether a pointer-motion event should be reported for this mode, given
/// whether a button is currently held. `MOUSE_MOTION` (1003) reports every
/// motion; `MOUSE_DRAG` (1002) reports motion only while a button is down; plain
/// `MOUSE_REPORT_CLICK` (1000) never reports motion.
pub fn reports_motion(mode: TermMode, button_held: bool) -> bool {
    if mode.contains(TermMode::MOUSE_MOTION) {
        true
    } else if mode.contains(TermMode::MOUSE_DRAG) {
        button_held
    } else {
        false
    }
}

/// Map a gpui mouse button to the VT encoder's button. Navigation (back/forward)
/// buttons are not part of the xterm mouse protocol, so they are not reported.
pub fn vt_button(button: gpui::MouseButton) -> Option<VtButton> {
    match button {
        gpui::MouseButton::Left => Some(VtButton::Left),
        gpui::MouseButton::Middle => Some(VtButton::Middle),
        gpui::MouseButton::Right => Some(VtButton::Right),
        gpui::MouseButton::Navigate(_) => None,
    }
}

/// Build the VT report modifiers from gpui's. Only shift/alt/ctrl affect a mouse
/// report; `super`/Command never appears in the wire encoding. (In practice the
/// view only reports when shift is *not* held — shift is the local-selection
/// override — so the shift bit is passed through honestly and is simply false on
/// the reporting path.)
pub fn report_modifiers(m: gpui::Modifiers) -> VtModifiers {
    VtModifiers::new(m.shift, m.alt, m.control, false)
}

/// Hit-test a grid-relative pixel offset to a zero-based `(col, row)` cell,
/// clamped to the grid. `rel_x` is measured from the grid's left edge and
/// `rel_y` from the top of grid row 0 (`grid_top_y`, the top-anchored origin).
/// A click in the surrounding padding / bottom remainder clamps to the nearest
/// edge cell, so the caller always gets a valid cell.
pub fn cell_from_offset(
    rel_x: f32,
    rel_y: f32,
    cell_w: f32,
    cell_h: f32,
    cols: usize,
    rows: usize,
) -> (usize, usize) {
    let col_f = if cell_w > 0.0 { (rel_x / cell_w).floor() } else { 0.0 };
    let row_f = if cell_h > 0.0 { (rel_y / cell_h).floor() } else { 0.0 };
    let col = col_f.clamp(0.0, cols.saturating_sub(1) as f32) as usize;
    let row = row_f.clamp(0.0, rows.saturating_sub(1) as f32) as usize;
    (col, row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reporting_active_tracks_any_mouse_mode() {
        assert!(!reporting_active(TermMode::NONE));
        assert!(reporting_active(TermMode::MOUSE_REPORT_CLICK));
        assert!(reporting_active(TermMode::MOUSE_DRAG));
        assert!(reporting_active(TermMode::MOUSE_MOTION));
        // SGR/UTF-8 are encoding selectors, not tracking modes on their own.
        assert!(!reporting_active(TermMode::SGR_MOUSE));
    }

    #[test]
    fn protocol_prefers_sgr_then_utf8_then_x10() {
        assert_eq!(protocol(TermMode::MOUSE_REPORT_CLICK), MouseProtocol::X10);
        assert_eq!(
            protocol(TermMode::MOUSE_REPORT_CLICK | TermMode::UTF8_MOUSE),
            MouseProtocol::Utf8
        );
        assert_eq!(
            protocol(TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE),
            MouseProtocol::Sgr
        );
        // SGR wins when both selectors are somehow set.
        assert_eq!(
            protocol(TermMode::SGR_MOUSE | TermMode::UTF8_MOUSE),
            MouseProtocol::Sgr
        );
    }

    #[test]
    fn motion_reporting_matches_tracking_mode() {
        // 1000: never report motion.
        assert!(!reports_motion(TermMode::MOUSE_REPORT_CLICK, true));
        assert!(!reports_motion(TermMode::MOUSE_REPORT_CLICK, false));
        // 1002: only while a button is held.
        assert!(reports_motion(TermMode::MOUSE_DRAG, true));
        assert!(!reports_motion(TermMode::MOUSE_DRAG, false));
        // 1003: always.
        assert!(reports_motion(TermMode::MOUSE_MOTION, true));
        assert!(reports_motion(TermMode::MOUSE_MOTION, false));
    }

    #[test]
    fn vt_button_maps_primary_three_only() {
        assert_eq!(vt_button(gpui::MouseButton::Left), Some(VtButton::Left));
        assert_eq!(vt_button(gpui::MouseButton::Middle), Some(VtButton::Middle));
        assert_eq!(vt_button(gpui::MouseButton::Right), Some(VtButton::Right));
        assert_eq!(
            vt_button(gpui::MouseButton::Navigate(gpui::NavigationDirection::Back)),
            None
        );
    }

    #[test]
    fn report_modifiers_drops_super() {
        let m = gpui::Modifiers {
            control: true,
            alt: true,
            shift: false,
            platform: true,
            function: false,
        };
        let vt = report_modifiers(m);
        assert!(vt.ctrl);
        assert!(vt.alt);
        assert!(!vt.shift);
        assert!(!vt.super_);
    }

    #[test]
    fn cell_from_offset_maps_and_clamps() {
        // 8px cell width, 16px cell height, 80x24 grid.
        // Middle of column 3, row 2.
        let (c, r) = cell_from_offset(3.0 * 8.0 + 4.0, 2.0 * 16.0 + 8.0, 8.0, 16.0, 80, 24);
        assert_eq!((c, r), (3, 2));
        // Negative offsets clamp to the origin cell.
        assert_eq!(cell_from_offset(-5.0, -5.0, 8.0, 16.0, 80, 24), (0, 0));
        // Past the far edge clamps to the last cell.
        assert_eq!(cell_from_offset(10_000.0, 10_000.0, 8.0, 16.0, 80, 24), (79, 23));
        // Exact cell boundary belongs to the higher-index cell (floor).
        assert_eq!(cell_from_offset(8.0, 16.0, 8.0, 16.0, 80, 24), (1, 1));
    }
}
