//! The plain, gpui-free key/modifier input types the encoders consume.
//!
//! These deliberately mirror only what a terminal encoder needs — a logical
//! key (a Unicode-producing character or a named functional key), the modifier
//! state, the physical location (left/right/numpad, needed for the kitty
//! modifiers-as-functional-keys reports), the press/repeat/release event type,
//! and the optional layout-resolved text + alternate-key codepoints. The R5
//! event-edge slice translates gpui `Keystroke`/`KeyDownEvent` plus the
//! `[NSApp currentEvent].keyCode` side-channel into a [`KeyInput`]; nothing in
//! this crate depends on gpui or AppKit.

/// The kitty keyboard-protocol event type. Values match the protocol's
/// `press`=1 / `repeat`=2 / `release`=3 encoding used in the `:N` event field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEventType {
    /// First press of the key.
    Press,
    /// Auto-repeat while the key is held.
    Repeat,
    /// Key release.
    Release,
}

impl KeyEventType {
    /// The numeric value carried in the kitty `;mods:N` event-type field.
    pub fn kitty_value(self) -> u8 {
        match self {
            KeyEventType::Press => 1,
            KeyEventType::Repeat => 2,
            KeyEventType::Release => 3,
        }
    }
}

/// Physical location of a key. Only the distinctions terminal encoders act on
/// are modelled: numpad keys get their own kitty functional codepoints, and
/// left/right disambiguates the modifier-as-functional-key reports
/// (`57441` left-shift vs `57447` right-shift, etc.).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyLocation {
    /// The ordinary main-cluster location.
    Standard,
    /// Left-hand instance of a paired key (used for modifier reports).
    Left,
    /// Right-hand instance of a paired key.
    Right,
    /// The numeric keypad.
    Numpad,
}

/// Modifier state at the time of a keystroke.
///
/// Only the four modifiers the kitty/xterm encoders act on are modelled:
/// `shift`, `alt` (Option on macOS), `ctrl`, and `super` (Command on macOS —
/// the Cmd-as-super `ESC[99;9u` path). The kitty numeric encoding assigns bit
/// values shift=1, alt=2, ctrl=4, super=8 and transmits `sum + 1`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    /// Shift.
    pub shift: bool,
    /// Alt / Option.
    pub alt: bool,
    /// Control.
    pub ctrl: bool,
    /// Super / Command (macOS ⌘). Encoded as the kitty `super` modifier.
    pub super_: bool,
}

impl Modifiers {
    /// No modifiers held.
    pub const NONE: Modifiers = Modifiers { shift: false, alt: false, ctrl: false, super_: false };

    /// Convenience constructor.
    pub fn new(shift: bool, alt: bool, ctrl: bool, super_: bool) -> Modifiers {
        Modifiers { shift, alt, ctrl, super_ }
    }

    /// Whether no modifier is held.
    pub fn is_empty(self) -> bool {
        !(self.shift || self.alt || self.ctrl || self.super_)
    }

    /// Whether the only modifier possibly held is shift (i.e. no ctrl/alt/super).
    /// Under the kitty disambiguate rule a printable key with at most shift held
    /// still emits its text; anything else forces a CSI-u sequence.
    pub fn only_shift_or_none(self) -> bool {
        !(self.alt || self.ctrl || self.super_)
    }

    /// Whether any of the non-shift modifiers (ctrl/alt/super) is held.
    pub fn has_non_shift(self) -> bool {
        self.alt || self.ctrl || self.super_
    }

    /// The kitty modifier bit-sum (shift=1, alt=2, ctrl=4, super=8), before the
    /// protocol's `+ 1`.
    pub fn kitty_bits(self) -> u32 {
        let mut bits = 0;
        if self.shift {
            bits |= 1;
        }
        if self.alt {
            bits |= 2;
        }
        if self.ctrl {
            bits |= 4;
        }
        if self.super_ {
            bits |= 8;
        }
        bits
    }
}

/// A named (functional) key: everything that is not a printable Unicode scalar.
///
/// Ported from the SwiftTerm fork's `KittyFunctionalKey` (ours — the behavior
/// reference for what Nice ships today) and cross-checked against alacritty's
/// `keyboard.rs` codepoint tables (Apache-2.0). The left/right modifier
/// variants carry the kitty modifiers-as-functional-keys reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum NamedKey {
    // Control / whitespace keys.
    Escape,
    Enter,
    Tab,
    Backspace,
    Space,
    // Editing / navigation.
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    // Function keys F1..=F35.
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
    F21,
    F22,
    F23,
    F24,
    F25,
    F26,
    F27,
    F28,
    F29,
    F30,
    F31,
    F32,
    F33,
    F34,
    F35,
    // Misc functional.
    Menu,
    CapsLock,
    ScrollLock,
    NumLock,
    PrintScreen,
    Pause,
    // Keypad.
    Keypad0,
    Keypad1,
    Keypad2,
    Keypad3,
    Keypad4,
    Keypad5,
    Keypad6,
    Keypad7,
    Keypad8,
    Keypad9,
    KeypadDecimal,
    KeypadDivide,
    KeypadMultiply,
    KeypadSubtract,
    KeypadAdd,
    KeypadEnter,
    KeypadEqual,
    KeypadSeparator,
    KeypadLeft,
    KeypadRight,
    KeypadUp,
    KeypadDown,
    KeypadPageUp,
    KeypadPageDown,
    KeypadHome,
    KeypadEnd,
    KeypadInsert,
    KeypadDelete,
    KeypadBegin,
    // Media keys.
    MediaPlay,
    MediaPause,
    MediaPlayPause,
    MediaReverse,
    MediaStop,
    MediaFastForward,
    MediaRewind,
    MediaTrackNext,
    MediaTrackPrevious,
    MediaRecord,
    VolumeDown,
    VolumeUp,
    VolumeMute,
    // Modifier keys (only reported in report-all-keys mode). The left/right split
    // is carried by the variant itself (ShiftLeft vs ShiftRight, each with its own
    // codepoint), not the location field; these appear when a bare modifier is
    // pressed/released, fed from the flagsChanged keyCode side-channel.
    ShiftLeft,
    ShiftRight,
    ControlLeft,
    ControlRight,
    AltLeft,
    AltRight,
    SuperLeft,
    SuperRight,
    HyperLeft,
    HyperRight,
    MetaLeft,
    MetaRight,
    IsoLevel3Shift,
    IsoLevel5Shift,
}

impl NamedKey {
    /// Whether this is a bare modifier key (reported as a functional key only in
    /// report-all-keys mode, and suppressed to modifier-only reports while an
    /// IME is composing).
    pub fn is_modifier(self) -> bool {
        matches!(
            self,
            NamedKey::ShiftLeft
                | NamedKey::ShiftRight
                | NamedKey::ControlLeft
                | NamedKey::ControlRight
                | NamedKey::AltLeft
                | NamedKey::AltRight
                | NamedKey::SuperLeft
                | NamedKey::SuperRight
                | NamedKey::HyperLeft
                | NamedKey::HyperRight
                | NamedKey::MetaLeft
                | NamedKey::MetaRight
                | NamedKey::IsoLevel3Shift
                | NamedKey::IsoLevel5Shift
        )
    }
}

/// The logical key: a printable Unicode scalar or a named functional key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// A key that produces a Unicode scalar. This is the unshifted / base
    /// character the CSI-u codepoint is derived from (`c` for both `c` and `C`);
    /// the shifted form, when relevant, rides [`KeyInput::shifted_key`].
    Char(char),
    /// A named functional key.
    Named(NamedKey),
}

/// One fully-resolved key event handed to the encoder.
///
/// The event-edge slice fills this in from gpui + the NSEvent keyCode
/// side-channel. The encoder is a pure function of this value plus the active
/// [`crate::keyboard::KittyFlags`] and [`crate::keyboard::KeyEncoder`] config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInput {
    /// The logical key.
    pub key: Key,
    /// Modifier state.
    pub modifiers: Modifiers,
    /// Physical location (numpad / left / right disambiguation).
    pub location: KeyLocation,
    /// Press / repeat / release.
    pub event: KeyEventType,
    /// The text the OS would insert for this key, if any. For a plain printable
    /// this is `Some("a")`; for a Command-modified or purely-functional key it
    /// is `None`. Used for the legacy/plain fast path and kitty associated-text
    /// reporting. Control characters here are ignored for associated-text.
    pub text: Option<String>,
    /// The shifted-form codepoint for kitty alternate-key reporting (e.g. `!`
    /// for the `1` key). `None` when not applicable or not requested.
    pub shifted_key: Option<char>,
    /// The base-layout codepoint for kitty alternate-key reporting (the key at
    /// this position on the default layout). `None` when unknown.
    pub base_layout_key: Option<char>,
    /// Whether an IME composition is currently in progress. While composing, the
    /// encoder suppresses everything except bare modifier keys in
    /// report-all-keys mode (kitty's composition rule).
    pub composing: bool,
}

impl KeyInput {
    /// A minimal press event for `key` with the given modifiers and no text —
    /// the common shape for functional keys and modifier-bearing keys. Tests and
    /// the edge use the field-init form for the rest.
    pub fn press(key: Key, modifiers: Modifiers) -> KeyInput {
        KeyInput {
            key,
            modifiers,
            location: KeyLocation::Standard,
            event: KeyEventType::Press,
            text: None,
            shifted_key: None,
            base_layout_key: None,
            composing: false,
        }
    }

    /// A plain printable press carrying its inserted `text`.
    pub fn text_press(ch: char, modifiers: Modifiers) -> KeyInput {
        KeyInput {
            key: Key::Char(ch),
            modifiers,
            location: KeyLocation::Standard,
            event: KeyEventType::Press,
            text: Some(ch.to_string()),
            shifted_key: None,
            base_layout_key: None,
            composing: false,
        }
    }
}
