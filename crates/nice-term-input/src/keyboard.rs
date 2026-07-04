//! The keyboard encoder: kitty CSI-u (with the full progressive-enhancement
//! flag ladder) plus the legacy VT fallback, in one unified pass driven by the
//! active [`KittyFlags`].
//!
//! This is a clean-room Rust implementation guided by two license-permitted
//! references (Ground rules): the SwiftTerm fork's `KittyKeyboardEncoder`
//! (ours — the exact behavior Nice ships today, incl. Cmd-as-super `ESC[99;9u`)
//! and alacritty's `keyboard.rs` (Apache-2.0 — the canonical CSI-u codepoint
//! tables and the disambiguate rule). Zed's GPL terminal crates are never
//! consulted.
//!
//! # The flag ladder
//!
//! [`KittyFlags`] models the kitty keyboard protocol's five progressive
//! enhancement bits. With **no** flags set the encoder emits the legacy VT
//! encoding (raw text, C0 control chars, `ESC[…`/`ESC O …` functional keys).
//! As flags are added the output climbs the ladder: `DISAMBIGUATE` moves
//! ambiguous/modified keys to CSI-u, `REPORT_EVENT_TYPES` adds the `:N`
//! press/repeat/release field, `REPORT_ALTERNATE_KEYS` adds the shifted/base
//! codepoints, `REPORT_ALL_KEYS` encodes every key (incl. bare modifiers) as
//! CSI-u, and `REPORT_ASSOCIATED_TEXT` appends the inserted text codepoints.
//!
//! # Divergence from the SwiftTerm fast path (intentional)
//!
//! SwiftTerm's non-report-all fast path bails to raw text whenever only
//! alt/ctrl are absent, which lets a `super`-modified printable slip through as
//! text. This encoder instead follows alacritty's disambiguate rule: under
//! `DISAMBIGUATE`, a printable with **any** non-shift modifier (ctrl/alt/super)
//! is emitted as CSI-u. That is what makes Cmd+C encode as `ESC[99;9u` — the
//! Cmd-as-super contract the roadmap (T8) and the memory note require.

use crate::key::{Key, KeyEventType, KeyInput, Modifiers, NamedKey};

/// C0 control bytes the legacy encoder emits (mirrors SwiftTerm `ControlCodes`).
mod cc {
    pub const BS: u8 = 0x08;
    pub const HT: u8 = 0x09;
    pub const CR: u8 = 0x0d;
    pub const ESC: u8 = 0x1b;
    pub const DEL: u8 = 0x7f;
}

const CSI: [u8; 2] = [cc::ESC, b'['];

bitflags::bitflags! {
    /// The kitty keyboard protocol progressive-enhancement flags, as requested
    /// by the application via `CSI > flags u` / `CSI = flags ; mode u`. Empty ==
    /// legacy mode.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct KittyFlags: u8 {
        /// `0b1` — Disambiguate escape codes (ambiguous/modified keys → CSI-u).
        const DISAMBIGUATE = 0b0_0001;
        /// `0b10` — Report event types (press/repeat/release `:N` field).
        const REPORT_EVENT_TYPES = 0b0_0010;
        /// `0b100` — Report alternate keys (shifted / base-layout codepoints).
        const REPORT_ALTERNATE_KEYS = 0b0_0100;
        /// `0b1000` — Report all keys as escape codes (incl. bare modifiers).
        const REPORT_ALL_KEYS = 0b0_1000;
        /// `0b10000` — Report associated text codepoints.
        const REPORT_ASSOCIATED_TEXT = 0b1_0000;
    }
}

/// Configuration for the keyboard encoder that is not carried per-keystroke:
/// the active kitty flags plus two legacy behaviors (DECCKM application-cursor
/// mode, and whether Backspace sends `^H` rather than `^?`).
#[derive(Clone, Copy, Debug)]
pub struct KeyEncoder {
    /// Active kitty progressive-enhancement flags. Empty == legacy VT encoding.
    pub flags: KittyFlags,
    /// DECCKM application-cursor-keys mode: in legacy encoding the cursor keys
    /// (and Home/End) use `ESC O …` instead of `ESC [ …`.
    pub app_cursor: bool,
    /// Whether Backspace transmits `^H` (0x08) instead of the default `^?`
    /// (0x7f). Matches SwiftTerm's `backspaceSendsControlH`.
    pub backspace_sends_control_h: bool,
}

impl Default for KeyEncoder {
    fn default() -> KeyEncoder {
        KeyEncoder { flags: KittyFlags::empty(), app_cursor: false, backspace_sends_control_h: false }
    }
}

/// How a functional key maps into an escape sequence.
enum FnEncoding {
    /// `ESC [ <letter>` (or `ESC O <letter>` in legacy app-cursor/SS3 form).
    CsiLetter(u8),
    /// `ESC [ <number> ~`.
    CsiTilde(u16),
    /// `ESC [ <codepoint> u` (kitty functional codepoint).
    CsiU(u32),
}

impl KeyEncoder {
    /// Build a `KeyEncoder` from just the flags (legacy behaviors default off).
    pub fn new(flags: KittyFlags) -> KeyEncoder {
        KeyEncoder { flags, ..KeyEncoder::default() }
    }

    /// Encode one key event into the bytes to write to the pty, or `None` when
    /// the event produces nothing (e.g. a release with event-reporting off, or a
    /// key swallowed while composing).
    pub fn encode(&self, input: &KeyInput) -> Option<Vec<u8>> {
        let flags = self.flags;
        let all_keys = flags.contains(KittyFlags::REPORT_ALL_KEYS);
        let disambiguate = flags.contains(KittyFlags::DISAMBIGUATE) || all_keys;
        let events = flags.contains(KittyFlags::REPORT_EVENT_TYPES);
        let alternates = flags.contains(KittyFlags::REPORT_ALTERNATE_KEYS);
        let want_text = all_keys && flags.contains(KittyFlags::REPORT_ASSOCIATED_TEXT);
        let include_text = want_text
            && input.event != KeyEventType::Release
            && !modifiers_prevent_text(input.modifiers);

        // Release reporting is gated behind REPORT_EVENT_TYPES; and without
        // report-all-keys, releases of Enter/Tab/Backspace are never reported.
        if input.event == KeyEventType::Release && !events {
            return None;
        }
        if input.event == KeyEventType::Release && events && !all_keys {
            if let Key::Named(n) = input.key {
                if matches!(n, NamedKey::Enter | NamedKey::Tab | NamedKey::Backspace) {
                    return None;
                }
            }
        }

        // While composing, report only bare modifier keys, and only in
        // report-all-keys mode (kitty composition rule / IME suppression).
        if input.composing {
            match input.key {
                Key::Named(n) if all_keys && n.is_modifier() => {}
                _ => return None,
            }
        }

        // Bare modifier keys are reported only in report-all-keys mode.
        if !all_keys {
            if let Key::Named(n) = input.key {
                if n.is_modifier() {
                    return None;
                }
            }
        }

        if all_keys {
            return Some(match input.key {
                Key::Named(n) if matches!(n, NamedKey::Enter | NamedKey::Tab | NamedKey::Backspace) => {
                    self.encode_csi_u(input, functional_codepoint(n).unwrap_or(0), include_text, alternates, events)
                }
                Key::Named(n) => {
                    self.encode_functional(n, input, true, include_text, events, alternates)?
                }
                Key::Char(c) => {
                    let cp = unshifted_codepoint(c, input.modifiers);
                    self.encode_csi_u(input, cp, include_text, alternates, events)
                }
            });
        }

        // Not report-all-keys: disambiguate or legacy.
        match input.key {
            Key::Char(c) => {
                // Alternate-key reporting forces CSI-u for shifted/base keys even
                // on the otherwise-plain text path.
                let wants_alt_csi = alternates
                    && (input.modifiers.shift
                        || input.shifted_key.is_some()
                        || input.base_layout_key.is_some());

                if input.modifiers.only_shift_or_none() && !wants_alt_csi {
                    // Plain / shift-only printable: emit the inserted text on
                    // press and repeat (a held key repeats its character);
                    // releases produce nothing on this non-report-all path.
                    if input.event == KeyEventType::Release {
                        return None;
                    }
                    return Some(char_text_bytes(input));
                }

                if disambiguate {
                    let cp = unshifted_codepoint(c, input.modifiers);
                    Some(self.encode_csi_u(input, cp, false, alternates, events))
                } else {
                    // Legacy modified printable: ctrl → C0 control, alt → ESC
                    // prefix (Option-as-Meta).
                    legacy_char_sequence(c, input)
                }
            }
            Key::Named(n) => self.encode_functional(n, input, disambiguate, false, events, alternates),
        }
    }

    /// Encode a named/functional key, either as kitty CSI-u/CSI (`disambiguate`)
    /// or the legacy VT form.
    fn encode_functional(
        &self,
        key: NamedKey,
        input: &KeyInput,
        disambiguate: bool,
        include_text: bool,
        events: bool,
        alternates: bool,
    ) -> Option<Vec<u8>> {
        let mut mod_bits = input.modifiers.kitty_bits();
        // Modifier keys carry their own bit, set on press / cleared on release
        // (kitty applies the modifier state by keysym, not the post-event state).
        if let Some(bit) = modifier_key_bit(key) {
            if input.event == KeyEventType::Release {
                mod_bits &= !bit;
            } else {
                mod_bits |= bit;
            }
        }
        let include_type = events && input.event != KeyEventType::Press;
        let wants_mod_field = mod_bits != 0 || include_type;

        match key {
            NamedKey::Escape => {
                if disambiguate {
                    return Some(self.encode_csi_u(input, 27, false, alternates, events));
                }
                return legacy_special(key, input, self.backspace_sends_control_h);
            }
            NamedKey::Enter | NamedKey::Tab | NamedKey::Backspace => {
                if disambiguate && wants_mod_field {
                    let cp = functional_codepoint(key).unwrap_or(0);
                    return Some(self.encode_csi_u(input, cp, false, alternates, events));
                }
                return legacy_special(key, input, self.backspace_sends_control_h);
            }
            _ => {}
        }

        Some(match functional_encoding(key) {
            FnEncoding::CsiLetter(letter) => {
                if !disambiguate && !wants_mod_field {
                    if uses_ss3_legacy(key, self.app_cursor) {
                        vec![cc::ESC, b'O', letter]
                    } else {
                        let mut out = CSI.to_vec();
                        out.push(letter);
                        out
                    }
                } else {
                    build_csi_with_modifier(
                        1,
                        mod_bits,
                        include_type.then_some(input.event),
                        &[letter],
                        true,
                    )
                }
            }
            FnEncoding::CsiTilde(number) => build_csi_with_modifier(
                number,
                mod_bits,
                include_type.then_some(input.event),
                b"~",
                false,
            ),
            FnEncoding::CsiU(codepoint) => {
                self.encode_csi_u(input, codepoint, include_text, alternates, events)
            }
        })
    }

    /// Build a `ESC [ <codepoint>[:alt] [;mods[:event]] [;text] u` kitty CSI-u
    /// sequence.
    fn encode_csi_u(
        &self,
        input: &KeyInput,
        codepoint: u32,
        include_text: bool,
        alternates: bool,
        events: bool,
    ) -> Vec<u8> {
        let mut mod_bits = input.modifiers.kitty_bits();
        if let Key::Named(n) = input.key {
            if let Some(bit) = modifier_key_bit(n) {
                if input.event == KeyEventType::Release {
                    mod_bits &= !bit;
                } else {
                    mod_bits |= bit;
                }
            }
        }
        let include_type = events && input.event != KeyEventType::Press;
        let include_mod_field = include_type || mod_bits != 0;

        let mut body = codepoint.to_string();

        if alternates {
            let shifted = if input.modifiers.shift { input.shifted_key } else { None };
            let base = input.base_layout_key;
            if shifted.is_some() || base.is_some() {
                body.push(':');
                if let Some(s) = shifted {
                    body.push_str(&(s as u32).to_string());
                }
                if let Some(b) = base {
                    body.push(':');
                    body.push_str(&(b as u32).to_string());
                }
            }
        }

        if include_mod_field {
            let mod_value = mod_bits + 1;
            if include_type {
                body.push_str(&format!(";{}:{}", mod_value, input.event.kitty_value()));
            } else {
                body.push_str(&format!(";{}", mod_value));
            }
        }

        if include_text {
            if let Some(cps) = text_codepoints(input.text.as_deref()) {
                if include_mod_field {
                    body.push(';');
                } else {
                    body.push_str(";;");
                }
                let joined: Vec<String> = cps.iter().map(|c| c.to_string()).collect();
                body.push_str(&joined.join(":"));
            }
        }

        let mut out = CSI.to_vec();
        out.extend_from_slice(body.as_bytes());
        out.push(b'u');
        out
    }
}

/// Build `ESC [ <number>[;mods[:event]] <terminator>`. When `omit_default_number`
/// is set the leading `1` is dropped if there is no modifier/event field
/// (so an unmodified cursor key is `ESC [ A`, not `ESC [ 1 A`).
fn build_csi_with_modifier(
    number: u16,
    mod_bits: u32,
    event_type: Option<KeyEventType>,
    terminator: &[u8],
    omit_default_number: bool,
) -> Vec<u8> {
    let include_field = mod_bits != 0 || event_type.is_some();
    let mut payload = String::new();
    if !omit_default_number || include_field || number != 1 {
        payload = number.to_string();
    }
    if include_field {
        if payload.is_empty() {
            payload = number.to_string();
        }
        let mod_value = mod_bits + 1;
        match event_type {
            Some(et) => payload.push_str(&format!(";{}:{}", mod_value, et.kitty_value())),
            None => payload.push_str(&format!(";{}", mod_value)),
        }
    }
    let mut out = CSI.to_vec();
    out.extend_from_slice(payload.as_bytes());
    out.extend_from_slice(terminator);
    out
}

/// The inserted-text bytes for a plain/shift printable: the OS-provided `text`
/// if present, else the key's own char.
fn char_text_bytes(input: &KeyInput) -> Vec<u8> {
    if let Some(text) = &input.text {
        if !text.is_empty() {
            return text.as_bytes().to_vec();
        }
    }
    match input.key {
        Key::Char(c) => c.to_string().into_bytes(),
        Key::Named(_) => Vec::new(),
    }
}

/// Legacy encoding for a modified printable (ctrl → C0 control byte, alt → ESC
/// prefix / Option-as-Meta). Returns `None` for combinations only the kitty
/// encoding can express (e.g. ctrl+shift, or a super-modified key).
fn legacy_char_sequence(c: char, input: &KeyInput) -> Option<Vec<u8>> {
    if input.event != KeyEventType::Press {
        return None;
    }
    let mods = input.modifiers;
    // super/hyper/meta have no legacy encoding.
    if mods.super_ {
        return None;
    }

    let mut out = Vec::new();
    if mods.alt {
        out.push(cc::ESC);
    }

    if mods.ctrl {
        // ctrl+shift has no distinct legacy control code: the shift is dropped and
        // the base key's ctrl mapping is sent (xterm / SwiftTerm parity), e.g.
        // Ctrl+Shift+C == Ctrl+C == 0x03. `c` is already the unshifted base char.
        let mapped = legacy_control_mapping(c)?;
        out.push(mapped);
        return Some(out);
    }

    // alt-only (Option-as-Meta): ESC + the character. Prefer the OS text so an
    // accented compose still rides through, else the base char.
    let ch = if mods.shift { input.shifted_key.unwrap_or(c) } else { c };
    if let Some(text) = &input.text {
        if !text.is_empty() {
            out.extend_from_slice(text.as_bytes());
            return Some(out);
        }
    }
    out.extend_from_slice(ch.to_string().as_bytes());
    Some(out)
}

/// The legacy Ctrl+<key> → C0 control byte mapping (SwiftTerm `legacyControlMapping`).
fn legacy_control_mapping(c: char) -> Option<u8> {
    let lower = c.to_ascii_lowercase();
    let mapped = match lower {
        ' ' => 0,
        '/' => 31,
        '0' => 48,
        '1' => 49,
        '2' => 0,
        '3' => 27,
        '4' => 28,
        '5' => 29,
        '6' => 30,
        '7' => 31,
        '8' => 127,
        '9' => 57,
        '?' => 127,
        '@' => 0,
        '[' => 27,
        '\\' => 28,
        ']' => 29,
        '^' => 30,
        '_' => 31,
        '~' => 30,
        'a'..='z' => 1 + (lower as u8 - b'a'),
        _ => return None,
    };
    Some(mapped)
}

/// Legacy VT sequence for a special control key (Enter/Escape/Backspace/Tab),
/// press-only, with the Option-as-Meta ESC prefix applied when alt is held.
fn legacy_special(key: NamedKey, input: &KeyInput, backspace_sends_control_h: bool) -> Option<Vec<u8>> {
    if input.event != KeyEventType::Press {
        return None;
    }
    let mods = input.modifiers;
    let seq: Vec<u8> = match key {
        NamedKey::Enter => vec![cc::CR],
        NamedKey::Escape => vec![cc::ESC],
        NamedKey::Backspace => {
            let base = if mods.ctrl || backspace_sends_control_h { cc::BS } else { cc::DEL };
            vec![base]
        }
        NamedKey::Tab => {
            if mods.shift {
                vec![cc::ESC, b'[', b'Z']
            } else {
                vec![cc::HT]
            }
        }
        _ => return None,
    };
    if mods.alt {
        let mut out = vec![cc::ESC];
        out.extend_from_slice(&seq);
        Some(out)
    } else {
        Some(seq)
    }
}

/// Whether a functional key uses the legacy `ESC O …` (SS3) form: F1–F4 always,
/// and the cursor/Home/End keys when application-cursor mode is on.
fn uses_ss3_legacy(key: NamedKey, app_cursor: bool) -> bool {
    match key {
        NamedKey::F1 | NamedKey::F2 | NamedKey::F3 | NamedKey::F4 => true,
        NamedKey::ArrowUp
        | NamedKey::ArrowDown
        | NamedKey::ArrowLeft
        | NamedKey::ArrowRight
        | NamedKey::Home
        | NamedKey::End => app_cursor,
        _ => false,
    }
}

/// The kitty modifier bit a bare modifier key sets/clears (shift=1, alt=2,
/// ctrl=4, super=8). `None` for non-modifier keys and the hyper/meta/iso keys
/// this encoder does not track as active modifiers.
fn modifier_key_bit(key: NamedKey) -> Option<u32> {
    match key {
        NamedKey::ShiftLeft | NamedKey::ShiftRight => Some(1),
        NamedKey::AltLeft | NamedKey::AltRight => Some(2),
        NamedKey::ControlLeft | NamedKey::ControlRight => Some(4),
        NamedKey::SuperLeft | NamedKey::SuperRight => Some(8),
        _ => None,
    }
}

/// Modifiers that suppress associated-text reporting (any non-shift modifier).
fn modifiers_prevent_text(mods: Modifiers) -> bool {
    mods.alt || mods.ctrl || mods.super_
}

/// The unshifted CSI-u codepoint for a character key. Kitty reports the base
/// (unshifted) codepoint; the shifted form rides the alternate-key field.
fn unshifted_codepoint(c: char, mods: Modifiers) -> u32 {
    if mods.shift {
        // Lowercase the ASCII/uppercase form to recover the base key.
        c.to_lowercase().next().unwrap_or(c) as u32
    } else {
        c as u32
    }
}

/// The associated-text codepoints, dropping C0/C1/DEL control scalars.
fn text_codepoints(text: Option<&str>) -> Option<Vec<u32>> {
    let text = text?;
    let cps: Vec<u32> = text
        .chars()
        .map(u32::from)
        .filter(|&v| !(v < 0x20 || (0x7f..=0x9f).contains(&v)))
        .collect();
    if cps.is_empty() {
        None
    } else {
        Some(cps)
    }
}

/// The kitty functional codepoint for Escape/Enter/Tab/Backspace and the CSI-u
/// functional keys. `None` for keys encoded as CSI-letter or CSI-tilde.
fn functional_codepoint(key: NamedKey) -> Option<u32> {
    match key {
        NamedKey::Escape => Some(27),
        NamedKey::Enter => Some(13),
        NamedKey::Tab => Some(9),
        NamedKey::Backspace => Some(127),
        NamedKey::Space => Some(32),
        _ => match functional_encoding(key) {
            FnEncoding::CsiU(cp) => Some(cp),
            _ => None,
        },
    }
}

/// The escape-sequence family for a functional key. Ported from SwiftTerm's
/// `functionalEncoding`, cross-checked with alacritty's kitty tables.
fn functional_encoding(key: NamedKey) -> FnEncoding {
    use NamedKey::*;
    match key {
        ArrowUp => FnEncoding::CsiLetter(b'A'),
        ArrowDown => FnEncoding::CsiLetter(b'B'),
        ArrowRight => FnEncoding::CsiLetter(b'C'),
        ArrowLeft => FnEncoding::CsiLetter(b'D'),
        Home => FnEncoding::CsiLetter(b'H'),
        End => FnEncoding::CsiLetter(b'F'),
        F1 => FnEncoding::CsiLetter(b'P'),
        F2 => FnEncoding::CsiLetter(b'Q'),
        // F3 in kitty diverges from terminfo: CSI 13 ~.
        F3 => FnEncoding::CsiTilde(13),
        F4 => FnEncoding::CsiLetter(b'S'),
        KeypadBegin => FnEncoding::CsiU(57427),
        Insert => FnEncoding::CsiTilde(2),
        Delete => FnEncoding::CsiTilde(3),
        PageUp => FnEncoding::CsiTilde(5),
        PageDown => FnEncoding::CsiTilde(6),
        F5 => FnEncoding::CsiTilde(15),
        F6 => FnEncoding::CsiTilde(17),
        F7 => FnEncoding::CsiTilde(18),
        F8 => FnEncoding::CsiTilde(19),
        F9 => FnEncoding::CsiTilde(20),
        F10 => FnEncoding::CsiTilde(21),
        F11 => FnEncoding::CsiTilde(23),
        F12 => FnEncoding::CsiTilde(24),
        Menu => FnEncoding::CsiU(57363),
        F13 => FnEncoding::CsiU(57376),
        F14 => FnEncoding::CsiU(57377),
        F15 => FnEncoding::CsiU(57378),
        F16 => FnEncoding::CsiU(57379),
        F17 => FnEncoding::CsiU(57380),
        F18 => FnEncoding::CsiU(57381),
        F19 => FnEncoding::CsiU(57382),
        F20 => FnEncoding::CsiU(57383),
        F21 => FnEncoding::CsiU(57384),
        F22 => FnEncoding::CsiU(57385),
        F23 => FnEncoding::CsiU(57386),
        F24 => FnEncoding::CsiU(57387),
        F25 => FnEncoding::CsiU(57388),
        F26 => FnEncoding::CsiU(57389),
        F27 => FnEncoding::CsiU(57390),
        F28 => FnEncoding::CsiU(57391),
        F29 => FnEncoding::CsiU(57392),
        F30 => FnEncoding::CsiU(57393),
        F31 => FnEncoding::CsiU(57394),
        F32 => FnEncoding::CsiU(57395),
        F33 => FnEncoding::CsiU(57396),
        F34 => FnEncoding::CsiU(57397),
        F35 => FnEncoding::CsiU(57398),
        CapsLock => FnEncoding::CsiU(57358),
        ScrollLock => FnEncoding::CsiU(57359),
        NumLock => FnEncoding::CsiU(57360),
        PrintScreen => FnEncoding::CsiU(57361),
        Pause => FnEncoding::CsiU(57362),
        Keypad0 => FnEncoding::CsiU(57399),
        Keypad1 => FnEncoding::CsiU(57400),
        Keypad2 => FnEncoding::CsiU(57401),
        Keypad3 => FnEncoding::CsiU(57402),
        Keypad4 => FnEncoding::CsiU(57403),
        Keypad5 => FnEncoding::CsiU(57404),
        Keypad6 => FnEncoding::CsiU(57405),
        Keypad7 => FnEncoding::CsiU(57406),
        Keypad8 => FnEncoding::CsiU(57407),
        Keypad9 => FnEncoding::CsiU(57408),
        KeypadDecimal => FnEncoding::CsiU(57409),
        KeypadDivide => FnEncoding::CsiU(57410),
        KeypadMultiply => FnEncoding::CsiU(57411),
        KeypadSubtract => FnEncoding::CsiU(57412),
        KeypadAdd => FnEncoding::CsiU(57413),
        KeypadEnter => FnEncoding::CsiU(57414),
        KeypadEqual => FnEncoding::CsiU(57415),
        KeypadSeparator => FnEncoding::CsiU(57416),
        KeypadLeft => FnEncoding::CsiU(57417),
        KeypadRight => FnEncoding::CsiU(57418),
        KeypadUp => FnEncoding::CsiU(57419),
        KeypadDown => FnEncoding::CsiU(57420),
        KeypadPageUp => FnEncoding::CsiU(57421),
        KeypadPageDown => FnEncoding::CsiU(57422),
        KeypadHome => FnEncoding::CsiU(57423),
        KeypadEnd => FnEncoding::CsiU(57424),
        KeypadInsert => FnEncoding::CsiU(57425),
        KeypadDelete => FnEncoding::CsiU(57426),
        MediaPlay => FnEncoding::CsiU(57428),
        MediaPause => FnEncoding::CsiU(57429),
        MediaPlayPause => FnEncoding::CsiU(57430),
        MediaReverse => FnEncoding::CsiU(57431),
        MediaStop => FnEncoding::CsiU(57432),
        MediaFastForward => FnEncoding::CsiU(57433),
        MediaRewind => FnEncoding::CsiU(57434),
        MediaTrackNext => FnEncoding::CsiU(57435),
        MediaTrackPrevious => FnEncoding::CsiU(57436),
        MediaRecord => FnEncoding::CsiU(57437),
        VolumeDown => FnEncoding::CsiU(57438),
        VolumeUp => FnEncoding::CsiU(57439),
        VolumeMute => FnEncoding::CsiU(57440),
        ShiftLeft => FnEncoding::CsiU(57441),
        ControlLeft => FnEncoding::CsiU(57442),
        AltLeft => FnEncoding::CsiU(57443),
        SuperLeft => FnEncoding::CsiU(57444),
        HyperLeft => FnEncoding::CsiU(57445),
        MetaLeft => FnEncoding::CsiU(57446),
        ShiftRight => FnEncoding::CsiU(57447),
        ControlRight => FnEncoding::CsiU(57448),
        AltRight => FnEncoding::CsiU(57449),
        SuperRight => FnEncoding::CsiU(57450),
        HyperRight => FnEncoding::CsiU(57451),
        MetaRight => FnEncoding::CsiU(57452),
        IsoLevel3Shift => FnEncoding::CsiU(57453),
        IsoLevel5Shift => FnEncoding::CsiU(57454),
        // Escape/Enter/Tab/Backspace/Space are handled before this table via
        // their dedicated codepoints; this arm keeps the match total.
        Escape | Enter | Tab | Backspace | Space => FnEncoding::CsiU(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::{Key, KeyEventType, KeyInput, KeyLocation, Modifiers, NamedKey};

    fn kitty(flags: KittyFlags) -> KeyEncoder {
        KeyEncoder::new(flags)
    }

    fn disamb() -> KeyEncoder {
        KeyEncoder::new(KittyFlags::DISAMBIGUATE)
    }

    fn char_input(c: char, mods: Modifiers, text: Option<&str>) -> KeyInput {
        KeyInput {
            key: Key::Char(c),
            modifiers: mods,
            location: KeyLocation::Standard,
            event: KeyEventType::Press,
            text: text.map(str::to_string),
            shifted_key: None,
            base_layout_key: None,
            composing: false,
        }
    }

    // ---- kitty CSI-u: the plan's representative matrix -----------------------

    #[test]
    fn plain_printable_sends_text_under_disambiguate() {
        // Plain 'a' with at most shift stays raw text even in disambiguate mode.
        let out = disamb().encode(&char_input('a', Modifiers::NONE, Some("a")));
        assert_eq!(out, Some(b"a".to_vec()));
    }

    #[test]
    fn shift_printable_sends_text() {
        let out = disamb().encode(&char_input('A', Modifiers::new(true, false, false, false), Some("A")));
        assert_eq!(out, Some(b"A".to_vec()));
    }

    #[test]
    fn ctrl_a_csi_u_under_disambiguate() {
        // ctrl+a -> CSI 97 ; 5 u  (base codepoint 97, ctrl bit 4 -> +1 = 5).
        let out = disamb().encode(&char_input('a', Modifiers::new(false, false, true, false), None));
        assert_eq!(out, Some(b"\x1b[97;5u".to_vec()));
    }

    #[test]
    fn alt_a_csi_u_under_disambiguate() {
        // alt bit 2 -> +1 = 3.
        let out = disamb().encode(&char_input('a', Modifiers::new(false, true, false, false), None));
        assert_eq!(out, Some(b"\x1b[97;3u".to_vec()));
    }

    #[test]
    fn cmd_c_encodes_as_super_csi_u() {
        // The Cmd-as-super contract (T8 / memory note): Cmd+C -> ESC[99;9u.
        // 'c' = 99, super bit 8 -> +1 = 9.
        let out = disamb().encode(&char_input('c', Modifiers::new(false, false, false, true), None));
        assert_eq!(out, Some(b"\x1b[99;9u".to_vec()));
    }

    #[test]
    fn ctrl_shift_combines_modifier_bits() {
        // ctrl+shift+a: base 97, bits shift(1)+ctrl(4)=5 -> +1 = 6.
        let out = disamb().encode(&char_input('a', Modifiers::new(true, false, true, false), None));
        assert_eq!(out, Some(b"\x1b[97;6u".to_vec()));
    }

    // ---- press / repeat / release -------------------------------------------

    #[test]
    fn event_types_press_repeat_release() {
        let enc = kitty(KittyFlags::REPORT_ALL_KEYS | KittyFlags::REPORT_EVENT_TYPES);
        let mk = |ev: KeyEventType| KeyInput {
            key: Key::Char('a'),
            modifiers: Modifiers::NONE,
            location: KeyLocation::Standard,
            event: ev,
            text: None,
            shifted_key: None,
            base_layout_key: None,
            composing: false,
        };
        // Press: default event omitted, no modifiers -> bare CSI-u.
        assert_eq!(enc.encode(&mk(KeyEventType::Press)), Some(b"\x1b[97u".to_vec()));
        // Repeat: ;1:2  (mods 0 -> +1 = 1, event 2).
        assert_eq!(enc.encode(&mk(KeyEventType::Repeat)), Some(b"\x1b[97;1:2u".to_vec()));
        // Release: ;1:3.
        assert_eq!(enc.encode(&mk(KeyEventType::Release)), Some(b"\x1b[97;1:3u".to_vec()));
    }

    #[test]
    fn plain_text_repeat_resends_but_release_drops() {
        // A held plain key repeats its character even with event reporting on;
        // the release produces nothing on the non-report-all path.
        let enc = kitty(KittyFlags::DISAMBIGUATE | KittyFlags::REPORT_EVENT_TYPES);
        let mut input = char_input('a', Modifiers::NONE, Some("a"));
        input.event = KeyEventType::Repeat;
        assert_eq!(enc.encode(&input), Some(b"a".to_vec()));
        input.event = KeyEventType::Release;
        assert_eq!(enc.encode(&input), None);
    }

    #[test]
    fn release_dropped_without_event_reporting() {
        let mut input = char_input('a', Modifiers::new(false, false, true, false), None);
        input.event = KeyEventType::Release;
        assert_eq!(disamb().encode(&input), None);
    }

    // ---- functional keys (kitty) --------------------------------------------

    #[test]
    fn arrow_up_plain_disambiguate() {
        let out = disamb().encode(&KeyInput::press(Key::Named(NamedKey::ArrowUp), Modifiers::NONE));
        assert_eq!(out, Some(b"\x1b[A".to_vec()));
    }

    #[test]
    fn arrow_up_shift_modified() {
        let out = disamb().encode(&KeyInput::press(
            Key::Named(NamedKey::ArrowUp),
            Modifiers::new(true, false, false, false),
        ));
        assert_eq!(out, Some(b"\x1b[1;2A".to_vec()));
    }

    #[test]
    fn f5_csi_tilde() {
        let out = disamb().encode(&KeyInput::press(Key::Named(NamedKey::F5), Modifiers::NONE));
        assert_eq!(out, Some(b"\x1b[15~".to_vec()));
    }

    #[test]
    fn f3_kitty_diverges_to_tilde_13() {
        let out = disamb().encode(&KeyInput::press(Key::Named(NamedKey::F3), Modifiers::NONE));
        assert_eq!(out, Some(b"\x1b[13~".to_vec()));
    }

    #[test]
    fn f13_kitty_codepoint() {
        let out = disamb().encode(&KeyInput::press(Key::Named(NamedKey::F13), Modifiers::NONE));
        assert_eq!(out, Some(b"\x1b[57376u".to_vec()));
    }

    // ---- modifiers-as-functional-keys (report-all-keys) ---------------------

    #[test]
    fn left_shift_reported_in_all_keys_mode() {
        let enc = kitty(KittyFlags::REPORT_ALL_KEYS | KittyFlags::REPORT_EVENT_TYPES);
        // Press left-shift: the key's own shift bit is applied -> ;2.
        let mut press = KeyInput::press(
            Key::Named(NamedKey::ShiftLeft),
            Modifiers::new(true, false, false, false),
        );
        press.location = KeyLocation::Left;
        assert_eq!(enc.encode(&press), Some(b"\x1b[57441;2u".to_vec()));
        // Release: the bit is cleared -> ;1, with event type :3.
        let mut release = press.clone();
        release.event = KeyEventType::Release;
        release.modifiers = Modifiers::NONE;
        assert_eq!(enc.encode(&release), Some(b"\x1b[57441;1:3u".to_vec()));
    }

    #[test]
    fn modifier_keys_suppressed_without_all_keys() {
        let out = disamb().encode(&KeyInput::press(
            Key::Named(NamedKey::ShiftLeft),
            Modifiers::new(true, false, false, false),
        ));
        assert_eq!(out, None);
    }

    #[test]
    fn composing_swallows_text_but_reports_modifiers_in_all_keys() {
        let enc = kitty(KittyFlags::REPORT_ALL_KEYS);
        // A composing character key is swallowed.
        let mut composing_char = char_input('a', Modifiers::NONE, Some("a"));
        composing_char.composing = true;
        assert_eq!(enc.encode(&composing_char), None);
        // A bare modifier still reports.
        let mut composing_mod = KeyInput::press(
            Key::Named(NamedKey::ControlLeft),
            Modifiers::new(false, false, true, false),
        );
        composing_mod.composing = true;
        assert_eq!(enc.encode(&composing_mod), Some(b"\x1b[57442;5u".to_vec()));
    }

    // ---- associated text ----------------------------------------------------

    #[test]
    fn associated_text_appended_in_all_keys_report_text() {
        let enc = kitty(
            KittyFlags::REPORT_ALL_KEYS | KittyFlags::REPORT_ASSOCIATED_TEXT,
        );
        // 'a' with no modifiers: CSI 97 ; ; 97 u  (no mod field -> ";;").
        let out = enc.encode(&char_input('a', Modifiers::NONE, Some("a")));
        assert_eq!(out, Some(b"\x1b[97;;97u".to_vec()));
    }

    // ---- alternate keys -----------------------------------------------------

    #[test]
    fn alternate_shifted_key_reported() {
        let enc = kitty(KittyFlags::DISAMBIGUATE | KittyFlags::REPORT_ALTERNATE_KEYS);
        // shift+1 -> '!': base 49, shifted 33, shift bit -> ;2. CSI 49:33;2u.
        let mut input = char_input('1', Modifiers::new(true, false, false, false), Some("!"));
        input.shifted_key = Some('!');
        assert_eq!(enc.encode(&input), Some(b"\x1b[49:33;2u".to_vec()));
    }

    // ---- legacy fallback (kitty off) ----------------------------------------

    #[test]
    fn legacy_plain_char() {
        let out = KeyEncoder::default().encode(&char_input('a', Modifiers::NONE, Some("a")));
        assert_eq!(out, Some(b"a".to_vec()));
    }

    #[test]
    fn legacy_ctrl_a_is_soh() {
        let out = KeyEncoder::default()
            .encode(&char_input('a', Modifiers::new(false, false, true, false), None));
        assert_eq!(out, Some(vec![0x01]));
    }

    #[test]
    fn legacy_ctrl_space_is_nul() {
        let out = KeyEncoder::default()
            .encode(&char_input(' ', Modifiers::new(false, false, true, false), None));
        assert_eq!(out, Some(vec![0x00]));
    }

    #[test]
    fn legacy_ctrl_bracket_is_esc() {
        let out = KeyEncoder::default()
            .encode(&char_input('[', Modifiers::new(false, false, true, false), None));
        assert_eq!(out, Some(vec![0x1b]));
    }

    #[test]
    fn legacy_ctrl_shift_letter_drops_shift_to_control_code() {
        // Ctrl+Shift+C in legacy mode has no distinct control code: it degrades to
        // Ctrl+C (0x03), matching xterm / SwiftTerm — not swallowed as zero bytes.
        let out = KeyEncoder::default()
            .encode(&char_input('c', Modifiers::new(true, false, true, false), None));
        assert_eq!(out, Some(vec![0x03]));
        // Ctrl+Shift+A == Ctrl+A == 0x01.
        let a = KeyEncoder::default()
            .encode(&char_input('a', Modifiers::new(true, false, true, false), None));
        assert_eq!(a, Some(vec![0x01]));
    }

    #[test]
    fn super_char_without_disambiguate_returns_none() {
        // ⌘C when only REPORT_EVENT_TYPES is set (no DISAMBIGUATE / REPORT_ALL_KEYS):
        // super has no legacy encoding, so the encoder yields nothing — the view
        // must keep ⌘C on the copy path (see input::kitty_forwards_super).
        let enc = kitty(KittyFlags::REPORT_EVENT_TYPES);
        let out = enc.encode(&char_input('c', Modifiers::new(false, false, false, true), None));
        assert_eq!(out, None);
    }

    #[test]
    fn legacy_alt_a_is_esc_prefixed() {
        let out = KeyEncoder::default()
            .encode(&char_input('a', Modifiers::new(false, true, false, false), Some("a")));
        assert_eq!(out, Some(vec![0x1b, b'a']));
    }

    #[test]
    fn legacy_arrow_up_plain_and_app_cursor() {
        let plain = KeyEncoder::default()
            .encode(&KeyInput::press(Key::Named(NamedKey::ArrowUp), Modifiers::NONE));
        assert_eq!(plain, Some(b"\x1b[A".to_vec()));

        let app = KeyEncoder { app_cursor: true, ..KeyEncoder::default() }
            .encode(&KeyInput::press(Key::Named(NamedKey::ArrowUp), Modifiers::NONE));
        assert_eq!(app, Some(b"\x1bOA".to_vec()));
    }

    #[test]
    fn legacy_f1_is_ss3() {
        let out = KeyEncoder::default()
            .encode(&KeyInput::press(Key::Named(NamedKey::F1), Modifiers::NONE));
        assert_eq!(out, Some(b"\x1bOP".to_vec()));
    }

    #[test]
    fn legacy_page_up_and_insert_delete() {
        let enc = KeyEncoder::default();
        assert_eq!(
            enc.encode(&KeyInput::press(Key::Named(NamedKey::PageUp), Modifiers::NONE)),
            Some(b"\x1b[5~".to_vec())
        );
        assert_eq!(
            enc.encode(&KeyInput::press(Key::Named(NamedKey::Insert), Modifiers::NONE)),
            Some(b"\x1b[2~".to_vec())
        );
        assert_eq!(
            enc.encode(&KeyInput::press(Key::Named(NamedKey::Delete), Modifiers::NONE)),
            Some(b"\x1b[3~".to_vec())
        );
    }

    #[test]
    fn legacy_special_keys() {
        let enc = KeyEncoder::default();
        assert_eq!(
            enc.encode(&KeyInput::press(Key::Named(NamedKey::Enter), Modifiers::NONE)),
            Some(vec![0x0d])
        );
        assert_eq!(
            enc.encode(&KeyInput::press(Key::Named(NamedKey::Tab), Modifiers::NONE)),
            Some(vec![0x09])
        );
        assert_eq!(
            enc.encode(&KeyInput::press(Key::Named(NamedKey::Escape), Modifiers::NONE)),
            Some(vec![0x1b])
        );
        // Backspace default -> DEL (^?); with control-h config -> BS (^H).
        assert_eq!(
            enc.encode(&KeyInput::press(Key::Named(NamedKey::Backspace), Modifiers::NONE)),
            Some(vec![0x7f])
        );
        let bs_h = KeyEncoder { backspace_sends_control_h: true, ..KeyEncoder::default() };
        assert_eq!(
            bs_h.encode(&KeyInput::press(Key::Named(NamedKey::Backspace), Modifiers::NONE)),
            Some(vec![0x08])
        );
    }

    #[test]
    fn legacy_shift_tab_is_csi_z() {
        let out = KeyEncoder::default().encode(&KeyInput::press(
            Key::Named(NamedKey::Tab),
            Modifiers::new(true, false, false, false),
        ));
        assert_eq!(out, Some(b"\x1b[Z".to_vec()));
    }

    #[test]
    fn legacy_alt_enter_is_esc_cr() {
        let out = KeyEncoder::default().encode(&KeyInput::press(
            Key::Named(NamedKey::Enter),
            Modifiers::new(false, true, false, false),
        ));
        assert_eq!(out, Some(vec![0x1b, 0x0d]));
    }
}
