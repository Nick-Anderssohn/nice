//! The VT mouse-report encoder: X10 (normal), SGR, and UTF-8 coordinate
//! encodings.
//!
//! Clean-room from the xterm mouse spec, guided by alacritty's
//! `input/mod.rs` mouse reporters (Apache-2.0): `normal_mouse_report` /
//! `sgr_mouse_report`, the `shift +4 / alt +8 / ctrl +16` modifier offsets, the
//! `32 + 1 + pos` coordinate byte, the `>= 95` two-byte UTF-8 fallback, and the
//! `223 / 2015` coordinate ceilings for the two byte-oriented encodings. The
//! caller supplies an already-hit-tested **cell** position (the pixel→cell
//! math using R4 grid metrics is the wiring slice's job); this encoder is a
//! pure function of cell coordinates, button, action, modifiers, and protocol.

use crate::key::Modifiers;

/// Which byte-level protocol the application selected for mouse reports.
///
/// The mode tracking that selects one of these lives in the terminal core; the
/// wiring slice maps the core's mode bits to this enum before calling the
/// encoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseProtocol {
    /// The original X10 encoding: `ESC [ M Cb Cx Cy`, each coordinate a single
    /// `32 + 1 + pos` byte (columns/rows past 223 are unreportable).
    X10,
    /// UTF-8 extended coordinates: like [`MouseProtocol::X10`], but a coordinate
    /// `>= 95` is written as its two-byte UTF-8 form (ceiling 2015).
    Utf8,
    /// SGR encoding: `ESC [ < Cb ; Cx ; Cy M|m`, decimal and unbounded — the
    /// only encoding that survives past column 223.
    Sgr,
}

/// A mouse button (or wheel direction). Base report codes follow xterm:
/// left/middle/right = 0/1/2, wheel = 64.. .
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    /// Left button (code 0).
    Left,
    /// Middle button (code 1).
    Middle,
    /// Right button (code 2).
    Right,
    /// No button — used for bare pointer motion (base code 3).
    None,
    /// Wheel scroll up (code 64).
    WheelUp,
    /// Wheel scroll down (code 65).
    WheelDown,
    /// Wheel tilt left (code 66).
    WheelLeft,
    /// Wheel tilt right (code 67).
    WheelRight,
}

impl MouseButton {
    /// The base report code before motion/modifier offsets.
    fn base_code(self) -> u16 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::None => 3,
            MouseButton::WheelUp => 64,
            MouseButton::WheelDown => 65,
            MouseButton::WheelLeft => 66,
            MouseButton::WheelRight => 67,
        }
    }
}

/// What the mouse did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    /// Button press (or a wheel tick — wheels report only as presses).
    Press,
    /// Button release. In X10/UTF-8 the button identity is lost (code 3); SGR
    /// preserves it and flips the terminator to lowercase `m`.
    Release,
    /// Pointer motion / drag (adds the `+32` motion bit to the report code).
    Motion,
}

/// One mouse event to encode: a hit-tested cell plus button/action/modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseInput {
    /// The button (or wheel direction).
    pub button: MouseButton,
    /// Press / release / motion.
    pub action: MouseAction,
    /// Zero-based column of the hit cell.
    pub col: usize,
    /// Zero-based row of the hit cell.
    pub line: usize,
    /// Modifier keys (only shift/alt/ctrl affect mouse reports).
    pub modifiers: Modifiers,
}

/// The `shift +4 / alt +8 / ctrl +16` modifier offset added to the report code.
fn modifier_offset(mods: Modifiers) -> u16 {
    let mut v = 0;
    if mods.shift {
        v += 4;
    }
    if mods.alt {
        v += 8;
    }
    if mods.ctrl {
        v += 16;
    }
    v
}

/// Encode a mouse event for `protocol`, or `None` when it is unreportable (an
/// X10/UTF-8 coordinate past the protocol ceiling).
pub fn encode_mouse(protocol: MouseProtocol, input: &MouseInput) -> Option<Vec<u8>> {
    let mods = modifier_offset(input.modifiers);
    let motion = if input.action == MouseAction::Motion { 32 } else { 0 };

    match protocol {
        MouseProtocol::Sgr => {
            // SGR keeps the button identity even on release; the terminator
            // carries press (`M`) vs release (`m`).
            let cb = input.button.base_code() + motion + mods;
            let terminator = if input.action == MouseAction::Release { b'm' } else { b'M' };
            let mut out =
                format!("\x1b[<{};{};{}", cb, input.col + 1, input.line + 1).into_bytes();
            out.push(terminator);
            Some(out)
        }
        MouseProtocol::X10 | MouseProtocol::Utf8 => {
            let utf8 = protocol == MouseProtocol::Utf8;
            let max = if utf8 { 2015 } else { 223 };
            if input.col >= max || input.line >= max {
                return None;
            }
            // Release loses the button identity in the byte encodings (code 3).
            let cb = if input.action == MouseAction::Release {
                3 + mods
            } else {
                input.button.base_code() + motion + mods
            };
            let mut out = vec![0x1b, b'[', b'M', (32 + cb) as u8];
            encode_coord(input.col, utf8, &mut out);
            encode_coord(input.line, utf8, &mut out);
            Some(out)
        }
    }
}

/// Append a single coordinate in the X10/UTF-8 byte form. A zero-based `pos`
/// becomes `32 + 1 + pos`; under UTF-8 a value `>= 95` (i.e. a wire byte past
/// 0x7f) is emitted as its two-byte UTF-8 encoding.
fn encode_coord(pos: usize, utf8: bool, out: &mut Vec<u8>) {
    if utf8 && pos >= 95 {
        let p = 32 + 1 + pos;
        out.push((0xC0 + p / 64) as u8);
        out.push((0x80 + (p & 63)) as u8);
    } else {
        out.push((32 + 1 + pos) as u8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(button: MouseButton, action: MouseAction, col: usize, line: usize) -> MouseInput {
        MouseInput { button, action, col, line, modifiers: Modifiers::NONE }
    }

    // ---- SGR ----------------------------------------------------------------

    #[test]
    fn sgr_left_press_origin() {
        let out = encode_mouse(
            MouseProtocol::Sgr,
            &input(MouseButton::Left, MouseAction::Press, 0, 0),
        );
        assert_eq!(out, Some(b"\x1b[<0;1;1M".to_vec()));
    }

    #[test]
    fn sgr_left_release_lowercase_m() {
        let out = encode_mouse(
            MouseProtocol::Sgr,
            &input(MouseButton::Left, MouseAction::Release, 0, 0),
        );
        assert_eq!(out, Some(b"\x1b[<0;1;1m".to_vec()));
    }

    #[test]
    fn sgr_beyond_223_columns() {
        // The >223-col case: SGR is decimal and unbounded.
        let out = encode_mouse(
            MouseProtocol::Sgr,
            &input(MouseButton::Left, MouseAction::Press, 299, 5),
        );
        assert_eq!(out, Some(b"\x1b[<0;300;6M".to_vec()));
    }

    #[test]
    fn sgr_ctrl_modifier_offset() {
        let mut i = input(MouseButton::Left, MouseAction::Press, 0, 0);
        i.modifiers = Modifiers::new(false, false, true, false);
        assert_eq!(encode_mouse(MouseProtocol::Sgr, &i), Some(b"\x1b[<16;1;1M".to_vec()));
    }

    #[test]
    fn sgr_wheel_up() {
        let out = encode_mouse(
            MouseProtocol::Sgr,
            &input(MouseButton::WheelUp, MouseAction::Press, 0, 0),
        );
        assert_eq!(out, Some(b"\x1b[<64;1;1M".to_vec()));
    }

    #[test]
    fn sgr_left_drag_sets_motion_bit() {
        let out = encode_mouse(
            MouseProtocol::Sgr,
            &input(MouseButton::Left, MouseAction::Motion, 5, 5),
        );
        assert_eq!(out, Some(b"\x1b[<32;6;6M".to_vec()));
    }

    // ---- X10 (normal) -------------------------------------------------------

    #[test]
    fn x10_left_press_origin() {
        let out = encode_mouse(
            MouseProtocol::X10,
            &input(MouseButton::Left, MouseAction::Press, 0, 0),
        );
        assert_eq!(out, Some(vec![0x1b, 0x5b, 0x4d, 0x20, 0x21, 0x21]));
    }

    #[test]
    fn x10_release_uses_code_three() {
        let out = encode_mouse(
            MouseProtocol::X10,
            &input(MouseButton::Left, MouseAction::Release, 0, 0),
        );
        // 32 + 3 = 35 = '#'.
        assert_eq!(out, Some(vec![0x1b, 0x5b, 0x4d, 0x23, 0x21, 0x21]));
    }

    #[test]
    fn x10_ctrl_modifier() {
        let mut i = input(MouseButton::Left, MouseAction::Press, 0, 0);
        i.modifiers = Modifiers::new(false, false, true, false);
        // 32 + 16 = 48 = '0'.
        assert_eq!(
            encode_mouse(MouseProtocol::X10, &i),
            Some(vec![0x1b, 0x5b, 0x4d, 0x30, 0x21, 0x21])
        );
    }

    #[test]
    fn x10_last_reportable_column() {
        // Column 222 -> 32 + 1 + 222 = 255 (0xff), the last single-byte column.
        let out = encode_mouse(
            MouseProtocol::X10,
            &input(MouseButton::Left, MouseAction::Press, 222, 0),
        );
        assert_eq!(out, Some(vec![0x1b, 0x5b, 0x4d, 0x20, 0xff, 0x21]));
    }

    #[test]
    fn x10_drops_column_past_ceiling() {
        let out = encode_mouse(
            MouseProtocol::X10,
            &input(MouseButton::Left, MouseAction::Press, 223, 0),
        );
        assert_eq!(out, None);
    }

    // ---- UTF-8 --------------------------------------------------------------

    #[test]
    fn utf8_two_byte_column() {
        // Column 100 (>= 95) -> two-byte UTF-8: 32+1+100 = 133 = U+0085 = C2 85.
        let out = encode_mouse(
            MouseProtocol::Utf8,
            &input(MouseButton::Left, MouseAction::Press, 100, 0),
        );
        assert_eq!(out, Some(vec![0x1b, 0x5b, 0x4d, 0x20, 0xc2, 0x85, 0x21]));
    }

    #[test]
    fn utf8_single_byte_below_threshold() {
        // Column 94 (< 95) stays single-byte: 32+1+94 = 127 = 0x7f.
        let out = encode_mouse(
            MouseProtocol::Utf8,
            &input(MouseButton::Left, MouseAction::Press, 94, 0),
        );
        assert_eq!(out, Some(vec![0x1b, 0x5b, 0x4d, 0x20, 0x7f, 0x21]));
    }

    #[test]
    fn utf8_drops_column_past_ceiling() {
        let out = encode_mouse(
            MouseProtocol::Utf8,
            &input(MouseButton::Left, MouseAction::Press, 2015, 0),
        );
        assert_eq!(out, None);
    }
}
