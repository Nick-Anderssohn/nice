//! The R5 event-edge: translate gpui key events + the macOS keyCode side-channel
//! into `nice-term-input`'s plain [`KeyInput`], and host the platform
//! [`InputHandler`] adapter that drives the IME.
//!
//! Nothing here reaches for `objc2` — that would be the design smell the crate
//! docs warn about. The one platform fact this edge needs, the layout-independent
//! hardware keyCode (gpui's [`Keystroke`] carries only `{modifiers, key,
//! key_char}` on the pin), is recovered through an **injected** callback
//! ([`KeyCodeProbe`]) the app builds in `crates/nice/src/platform` from
//! `[NSApp currentEvent].keyCode` — the same injection pattern as the
//! demand-present kick. This module only consumes the `Option<u16>` it returns.
//!
//! ## Two pure translations (unit-tested here, gpui aside)
//!
//! * [`encoder_config`] maps alacritty's tracked [`TermMode`] (the kitty
//!   progressive-enhancement bits + DECCKM app-cursor) onto a
//!   [`KeyEncoder`]/[`KittyFlags`] — this is how the app's `CSI > flags u`
//!   requests reach the encoder.
//! * [`build_key_input`] folds a gpui [`Keystroke`] + press/repeat/release +
//!   the keyCode into a [`KeyInput`]: functional keys by name, printables by
//!   char, the shifted/base-layout alternates the keyCode recovers.
//!
//! ## The InputHandler adapter
//!
//! [`TermInputHandler`] implements the **platform `InputHandler` trait DIRECTLY**
//! (not via `ElementInputHandler` — its blanket impl forwards
//! `prefers_ime_for_printable_keys` to `accepts_text_input`, and a terminal needs
//! `accepts_text_input = true` WITH `prefers_ime_for_printable_keys = false`).
//! Its methods are thin shells over [`TerminalView`]'s IME state (see the ime-
//! spike `TermInputHandler` this productionizes). The view registers it every
//! frame via `window.handle_input(&focus_handle, …)` during the element's paint.

use std::ops::Range;

use alacritty_terminal::term::TermMode;
use gpui::{App, Bounds, Entity, InputHandler, Pixels, Point, UTF16Selection, Window};

use nice_term_input::{
    Key, KeyEncoder, KeyEventType, KeyInput, KeyLocation, KittyFlags, Modifiers, NamedKey,
};

use crate::view::TerminalView;

/// The injected macOS keyCode side-channel: returns `[NSApp currentEvent].keyCode`
/// for the key event currently being dispatched (or `None` when the current event
/// is not a key event / there is no current event). Built in
/// `crates/nice/src/platform` (the sole objc2 home) and handed to
/// [`TerminalView::set_keycode_probe`]; this crate stays objc2-free.
pub type KeyCodeProbe = std::sync::Arc<dyn Fn() -> Option<u16>>;

/// Build the [`KeyEncoder`] config from the terminal's currently-tracked
/// [`TermMode`]. The kitty progressive-enhancement flags the app requested (via
/// `CSI > flags u` / `CSI = flags ; mode u`, which alacritty tracks as the
/// `*_ESC_CODES` / `REPORT_*` mode bits) map one-to-one onto [`KittyFlags`], and
/// DECCKM application-cursor mode maps onto the legacy SS3 cursor-key path.
pub fn encoder_config(mode: TermMode, backspace_sends_control_h: bool) -> KeyEncoder {
    let mut flags = KittyFlags::empty();
    if mode.contains(TermMode::DISAMBIGUATE_ESC_CODES) {
        flags |= KittyFlags::DISAMBIGUATE;
    }
    if mode.contains(TermMode::REPORT_EVENT_TYPES) {
        flags |= KittyFlags::REPORT_EVENT_TYPES;
    }
    if mode.contains(TermMode::REPORT_ALTERNATE_KEYS) {
        flags |= KittyFlags::REPORT_ALTERNATE_KEYS;
    }
    if mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) {
        flags |= KittyFlags::REPORT_ALL_KEYS;
    }
    if mode.contains(TermMode::REPORT_ASSOCIATED_TEXT) {
        flags |= KittyFlags::REPORT_ASSOCIATED_TEXT;
    }
    KeyEncoder {
        flags,
        app_cursor: mode.contains(TermMode::APP_CURSOR),
        backspace_sends_control_h,
    }
}

/// Whether any kitty progressive-enhancement flag is active. When it is, ⌘-keyed
/// and plain printables are encoded to the pty (the T8 `ESC[99;9u` contract);
/// when it is not, ⌘ is left for app keybindings / copy-paste (slice 3) and
/// printables ride the IME `insertText` path.
pub fn kitty_active(mode: TermMode) -> bool {
    mode.intersects(
        TermMode::DISAMBIGUATE_ESC_CODES
            | TermMode::REPORT_EVENT_TYPES
            | TermMode::REPORT_ALTERNATE_KEYS
            | TermMode::REPORT_ALL_KEYS_AS_ESC
            | TermMode::REPORT_ASSOCIATED_TEXT,
    )
}

/// Whether a ⌘/super-modified key would actually be **forwarded to the pty** as a
/// kitty CSI-u sequence (the `ESC[99;9u` contract) rather than left for macOS app
/// shortcuts / copy-paste. This is narrower than [`kitty_active`]: the encoder
/// only lifts a super-modified printable to CSI-u under `DISAMBIGUATE` (or
/// `REPORT_ALL_KEYS`); the report-event-types / alternate-keys / associated-text
/// bits alone leave ⌘ on the legacy path, where super has no encoding. Gating ⌘C
/// on [`kitty_active`] instead would strand it — the copy path skipped *and* the
/// encoder emitting nothing (`ESC[99;9u` requires `DISAMBIGUATE`). Use this for
/// every ⌘-vs-app-shortcut decision so the two sides never disagree.
pub fn kitty_forwards_super(mode: TermMode) -> bool {
    mode.intersects(TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_ALL_KEYS_AS_ESC)
}

/// Map a gpui [`Keystroke::key`] name to a functional [`NamedKey`], or `None` if
/// it is an ordinary printable (a `Key::Char`). gpui's macOS backend names these
/// exactly (`gpui_macos::events::parse_keystroke`): `"up"`, `"pagedown"`,
/// `"f5"`, … `"space"` is deliberately **not** here — it is a printable
/// `Key::Char(' ')` so plain Space rides `insertText`/the plain-text path and
/// ctrl+Space maps to NUL via the encoder.
pub fn named_key_for(key: &str) -> Option<NamedKey> {
    Some(match key {
        "escape" => NamedKey::Escape,
        "enter" => NamedKey::Enter,
        "tab" => NamedKey::Tab,
        "backspace" => NamedKey::Backspace,
        "up" => NamedKey::ArrowUp,
        "down" => NamedKey::ArrowDown,
        "left" => NamedKey::ArrowLeft,
        "right" => NamedKey::ArrowRight,
        "home" => NamedKey::Home,
        "end" => NamedKey::End,
        "pageup" => NamedKey::PageUp,
        "pagedown" => NamedKey::PageDown,
        "insert" => NamedKey::Insert,
        "delete" => NamedKey::Delete,
        "f1" => NamedKey::F1,
        "f2" => NamedKey::F2,
        "f3" => NamedKey::F3,
        "f4" => NamedKey::F4,
        "f5" => NamedKey::F5,
        "f6" => NamedKey::F6,
        "f7" => NamedKey::F7,
        "f8" => NamedKey::F8,
        "f9" => NamedKey::F9,
        "f10" => NamedKey::F10,
        "f11" => NamedKey::F11,
        "f12" => NamedKey::F12,
        "f13" => NamedKey::F13,
        "f14" => NamedKey::F14,
        "f15" => NamedKey::F15,
        "f16" => NamedKey::F16,
        "f17" => NamedKey::F17,
        "f18" => NamedKey::F18,
        "f19" => NamedKey::F19,
        "f20" => NamedKey::F20,
        "f21" => NamedKey::F21,
        "f22" => NamedKey::F22,
        "f23" => NamedKey::F23,
        "f24" => NamedKey::F24,
        "f25" => NamedKey::F25,
        "f26" => NamedKey::F26,
        "f27" => NamedKey::F27,
        "f28" => NamedKey::F28,
        "f29" => NamedKey::F29,
        "f30" => NamedKey::F30,
        "f31" => NamedKey::F31,
        "f32" => NamedKey::F32,
        "f33" => NamedKey::F33,
        "f34" => NamedKey::F34,
        "f35" => NamedKey::F35,
        _ => return None,
    })
}

/// Translate a gpui [`gpui::Modifiers`] to the encoder's [`Modifiers`]. macOS ⌘
/// is `platform` → the kitty `super` modifier (the Cmd-as-super path).
fn map_modifiers(m: &gpui::Modifiers) -> Modifiers {
    Modifiers {
        shift: m.shift,
        alt: m.alt,
        ctrl: m.control,
        super_: m.platform,
    }
}

/// Fold a gpui [`Keystroke`] + event kind + the recovered keyCode into a
/// [`KeyInput`] for the encoder. Returns `None` only for an empty, keyless
/// keystroke (nothing to encode).
///
/// * Functional keys ([`named_key_for`]) become `Key::Named`.
/// * Printables become `Key::Char` of the layout label gpui reports (`key`), with
///   `text` from `key_char` (the inserted string). The keyCode recovers the
///   `base_layout_key` (US-QWERTY codepoint at this physical key) for kitty
///   alternate reporting, and — when Shift is still attached (gpui keeps it for
///   a-z) — the `shifted_key`.
pub fn build_key_input(
    keystroke: &gpui::Keystroke,
    event: KeyEventType,
    keycode: Option<u16>,
    composing: bool,
) -> Option<KeyInput> {
    let modifiers = map_modifiers(&keystroke.modifiers);
    let location = keycode.map(keycode_location).unwrap_or(KeyLocation::Standard);

    if let Some(named) = named_key_for(&keystroke.key) {
        return Some(KeyInput {
            key: Key::Named(named),
            modifiers,
            location,
            event,
            text: None,
            shifted_key: None,
            base_layout_key: None,
            composing,
        });
    }

    // `"space"` is a printable (not a `named_key_for` entry), but its gpui key
    // *name* is the word "space", so `chars().next()` would wrongly yield 's'
    // (making Ctrl+Space encode 0x13/XOFF instead of NUL). Map the name back to
    // the space scalar; every other printable is a single-char name.
    let primary = if keystroke.key == "space" {
        ' '
    } else {
        keystroke.key.chars().next()?
    };
    let text = keystroke.key_char.clone();
    let shifted_key = if modifiers.shift {
        keystroke
            .key_char
            .as_deref()
            .and_then(|s| s.chars().next())
            .filter(|&c| c != primary)
    } else {
        None
    };
    let base_layout_key = keycode
        .and_then(us_layout_base_char)
        .filter(|&c| c != primary);

    Some(KeyInput {
        key: Key::Char(primary),
        modifiers,
        location,
        event,
        text,
        shifted_key,
        base_layout_key,
        composing,
    })
}

/// The physical [`KeyLocation`] a macOS virtual keyCode denotes. Only the numpad
/// is distinguished here: the encoder's left/right split matters only for bare
/// modifier keys, and those arrive through the flagsChanged path
/// ([`build_modifier_input`]) where the left/right key is baked into the
/// [`NamedKey`] variant, not this location field.
pub fn keycode_location(keycode: u16) -> KeyLocation {
    match keycode {
        // kVK_ANSI_Keypad* cluster (decimal/operators + digits 0–9).
        65 | 67 | 69 | 71 | 75 | 76 | 78 | 81 | 82..=92 => KeyLocation::Numpad,
        _ => KeyLocation::Standard,
    }
}

/// The bare-modifier [`NamedKey`] a macOS modifier virtual keyCode denotes, or
/// `None` if the keyCode is not a modifier key. This feeds the kitty
/// modifiers-as-functional-keys reports (report-all-keys mode) off the
/// flagsChanged keyCode side-channel. Left/right is carried by the keyCode itself
/// (`kVK_Shift` 56 vs `kVK_RightShift` 60, …) — exactly the distinction the
/// `57441` (left-shift) vs `57447` (right-shift) codepoints need.
pub fn modifier_named_key(keycode: u16) -> Option<NamedKey> {
    Some(match keycode {
        54 => NamedKey::SuperRight,   // kVK_RightCommand
        55 => NamedKey::SuperLeft,    // kVK_Command
        56 => NamedKey::ShiftLeft,    // kVK_Shift
        58 => NamedKey::AltLeft,      // kVK_Option
        59 => NamedKey::ControlLeft,  // kVK_Control
        60 => NamedKey::ShiftRight,   // kVK_RightShift
        61 => NamedKey::AltRight,     // kVK_RightOption
        62 => NamedKey::ControlRight, // kVK_RightControl
        _ => return None,
    })
}

/// Fold a flagsChanged transition into a bare-modifier [`KeyInput`], or `None`
/// when `keycode` is not a modifier key. The specific left/right key comes from
/// `keycode` ([`modifier_named_key`]); **press vs release is computed from the new
/// aggregate `modifiers`** — the key's modifier group is active immediately after
/// a press and inactive after the last release — so no per-key held-state is
/// remembered (the one edge this cannot split is holding both same-side keys and
/// releasing one, which it reports as a press). The encoder emits bytes for this
/// only in report-all-keys mode, and drops the release unless event-reporting is
/// on; this just builds the input and lets [`KeyEncoder::encode`] decide.
///
/// [`KeyEncoder::encode`]: nice_term_input::KeyEncoder::encode
pub fn build_modifier_input(
    keycode: u16,
    modifiers: &gpui::Modifiers,
    composing: bool,
) -> Option<KeyInput> {
    let named = modifier_named_key(keycode)?;
    let active = match named {
        NamedKey::ShiftLeft | NamedKey::ShiftRight => modifiers.shift,
        NamedKey::ControlLeft | NamedKey::ControlRight => modifiers.control,
        NamedKey::AltLeft | NamedKey::AltRight => modifiers.alt,
        NamedKey::SuperLeft | NamedKey::SuperRight => modifiers.platform,
        _ => return None,
    };
    let event = if active {
        KeyEventType::Press
    } else {
        KeyEventType::Release
    };
    Some(KeyInput {
        key: Key::Named(named),
        modifiers: map_modifiers(modifiers),
        location: KeyLocation::Standard,
        event,
        text: None,
        shifted_key: None,
        base_layout_key: None,
        composing,
    })
}

/// The US-QWERTY base character a macOS virtual keyCode maps to — the
/// layout-independent codepoint the kitty alternate-key field reports. `None` for
/// non-character keys (function keys, modifiers, navigation). This is a fixed
/// hardware→US table (the whole point of the keyCode side-channel: it does not
/// vary with the user's active layout).
pub fn us_layout_base_char(keycode: u16) -> Option<char> {
    Some(match keycode {
        0 => 'a',
        1 => 's',
        2 => 'd',
        3 => 'f',
        4 => 'h',
        5 => 'g',
        6 => 'z',
        7 => 'x',
        8 => 'c',
        9 => 'v',
        11 => 'b',
        12 => 'q',
        13 => 'w',
        14 => 'e',
        15 => 'r',
        16 => 'y',
        17 => 't',
        18 => '1',
        19 => '2',
        20 => '3',
        21 => '4',
        22 => '6',
        23 => '5',
        24 => '=',
        25 => '9',
        26 => '7',
        27 => '-',
        28 => '8',
        29 => '0',
        30 => ']',
        31 => 'o',
        32 => 'u',
        33 => '[',
        34 => 'i',
        35 => 'p',
        37 => 'l',
        38 => 'j',
        39 => '\'',
        40 => 'k',
        41 => ';',
        42 => '\\',
        43 => ',',
        44 => '/',
        45 => 'n',
        46 => 'm',
        47 => '.',
        50 => '`',
        // Keypad characters (base forms).
        65 => '.',
        67 => '*',
        69 => '+',
        75 => '/',
        78 => '-',
        81 => '=',
        82 => '0',
        83 => '1',
        84 => '2',
        85 => '3',
        86 => '4',
        87 => '5',
        88 => '6',
        89 => '7',
        91 => '8',
        92 => '9',
        _ => return None,
    })
}

/// The platform [`InputHandler`] adapter for a [`TerminalView`], implemented
/// directly on the trait (not via `ElementInputHandler`). Registered each frame
/// with `window.handle_input(&focus_handle, TermInputHandler { .. }, cx)` in the
/// element's paint; `element_bounds` is the grid element's bounds that frame, so
/// `bounds_for_range` can anchor the candidate window at the grid cursor cell.
pub struct TermInputHandler {
    /// The view whose IME state these callbacks read/drive.
    pub view: Entity<TerminalView>,
    /// The grid element's bounds this frame (for the candidate-window anchor).
    pub element_bounds: Bounds<Pixels>,
}

impl InputHandler for TermInputHandler {
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        // Never `None`: some IMEs misbehave on it. The document is the preedit,
        // so the selection is the preedit caret/selection (collapsed when idle).
        Some(UTF16Selection {
            range: self.view.read(cx).ime_selected_range_utf16(),
            reversed: false,
        })
    }

    fn marked_text_range(&mut self, _window: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        self.view.read(cx).ime_marked_range_utf16()
    }

    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<String> {
        let (text, actual) = self.view.read(cx).ime_text_for_range(range_utf16)?;
        *adjusted_range = Some(actual);
        Some(text)
    }

    fn replace_text_in_range(
        &mut self,
        replacement_range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.view
            .update(cx, |view, cx| view.ime_commit(replacement_range, text, cx));
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.view.update(cx, |view, cx| {
            view.ime_set_marked(range_utf16, new_text, new_selected_range, cx)
        });
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut App) {
        self.view.update(cx, |view, cx| view.ime_unmark(cx));
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        // ALWAYS `Some` — the zed#46055 fix. `None` makes gpui report
        // NSRect(0,0,0,0), which AppKit resolves to the screen's bottom-left.
        let element_bounds = self.element_bounds;
        Some(self.view.update(cx, |view, cx| {
            view.ime_anchor_bounds(range_utf16, element_bounds, window, cx)
        }))
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<usize> {
        // Minimal-but-total (low value for a terminal): must not panic or return
        // NSNotFound while composing. Point→cell hit-testing is R5 slice 3's job.
        Some(0)
    }

    fn apple_press_and_hold_enabled(&mut self) -> bool {
        // Terminal convention (iTerm2): a held key auto-repeats; no accent popover.
        false
    }

    fn accepts_text_input(&mut self, _window: &mut Window, _cx: &mut App) -> bool {
        // The IME must engage (CJK compose) — but paired with
        // `prefers_ime_for_printable_keys = false` so raw printables reach the pty.
        true
    }

    fn prefers_ime_for_printable_keys(&mut self, _window: &mut Window, _cx: &mut App) -> bool {
        // Zed terminal policy: raw printable keys reach the terminal process
        // rather than being routed to the IME before keybinding matching.
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Keystroke;

    fn ks(key: &str, key_char: Option<&str>, m: gpui::Modifiers) -> Keystroke {
        Keystroke {
            modifiers: m,
            key: key.to_string(),
            key_char: key_char.map(str::to_string),
        }
    }

    fn mods(shift: bool, alt: bool, ctrl: bool, platform: bool) -> gpui::Modifiers {
        gpui::Modifiers {
            control: ctrl,
            alt,
            shift,
            platform,
            function: false,
        }
    }

    #[test]
    fn encoder_config_maps_kitty_flags_and_app_cursor() {
        let none = encoder_config(TermMode::NONE, false);
        assert_eq!(none.flags, KittyFlags::empty());
        assert!(!none.app_cursor);

        let full = encoder_config(TermMode::KITTY_KEYBOARD_PROTOCOL | TermMode::APP_CURSOR, false);
        assert!(full.flags.contains(KittyFlags::DISAMBIGUATE));
        assert!(full.flags.contains(KittyFlags::REPORT_EVENT_TYPES));
        assert!(full.flags.contains(KittyFlags::REPORT_ALTERNATE_KEYS));
        assert!(full.flags.contains(KittyFlags::REPORT_ALL_KEYS));
        assert!(full.flags.contains(KittyFlags::REPORT_ASSOCIATED_TEXT));
        assert!(full.app_cursor);
    }

    #[test]
    fn kitty_active_tracks_any_enhancement_flag() {
        assert!(!kitty_active(TermMode::NONE));
        assert!(!kitty_active(TermMode::APP_CURSOR)); // DECCKM is not kitty
        assert!(kitty_active(TermMode::DISAMBIGUATE_ESC_CODES));
        assert!(kitty_active(TermMode::REPORT_ALL_KEYS_AS_ESC));
    }

    #[test]
    fn named_keys_map_from_gpui_names() {
        assert_eq!(named_key_for("enter"), Some(NamedKey::Enter));
        assert_eq!(named_key_for("left"), Some(NamedKey::ArrowLeft));
        assert_eq!(named_key_for("pagedown"), Some(NamedKey::PageDown));
        assert_eq!(named_key_for("f13"), Some(NamedKey::F13));
        // Space and letters are printables, not named keys.
        assert_eq!(named_key_for("space"), None);
        assert_eq!(named_key_for("a"), None);
    }

    #[test]
    fn build_plain_char_carries_text() {
        let input = build_key_input(&ks("a", Some("a"), mods(false, false, false, false)), KeyEventType::Press, Some(0), false).unwrap();
        assert_eq!(input.key, Key::Char('a'));
        assert_eq!(input.text.as_deref(), Some("a"));
        assert!(input.modifiers.is_empty());
        // keyCode 0 is US 'a' == primary, so no redundant base_layout_key.
        assert_eq!(input.base_layout_key, None);
    }

    #[test]
    fn build_cmd_c_is_super_modified_c() {
        // The T8 contract: gpui reports key "c", platform (⌘) held.
        let input = build_key_input(&ks("c", None, mods(false, false, false, true)), KeyEventType::Press, Some(8), false).unwrap();
        assert_eq!(input.key, Key::Char('c'));
        assert!(input.modifiers.super_);
        assert!(!input.modifiers.ctrl);
    }

    #[test]
    fn build_shift_letter_recovers_shifted_alternate() {
        // gpui keeps shift for a-z: key "a", shift true, key_char "A".
        let input = build_key_input(&ks("a", Some("A"), mods(true, false, false, false)), KeyEventType::Press, Some(0), false).unwrap();
        assert_eq!(input.key, Key::Char('a'));
        assert!(input.modifiers.shift);
        assert_eq!(input.shifted_key, Some('A'));
    }

    #[test]
    fn build_functional_key_has_no_text() {
        let input = build_key_input(&ks("left", None, mods(false, false, false, false)), KeyEventType::Repeat, None, false).unwrap();
        assert_eq!(input.key, Key::Named(NamedKey::ArrowLeft));
        assert_eq!(input.event, KeyEventType::Repeat);
        assert_eq!(input.text, None);
    }

    #[test]
    fn build_keypad_key_is_numpad_location() {
        // keyCode 87 = kVK_ANSI_Keypad5, gpui reports "5".
        let input = build_key_input(&ks("5", Some("5"), mods(false, false, false, false)), KeyEventType::Press, Some(87), false).unwrap();
        assert_eq!(input.location, KeyLocation::Numpad);
    }

    #[test]
    fn us_layout_base_covers_letters_digits_and_keypad() {
        assert_eq!(us_layout_base_char(8), Some('c'));
        assert_eq!(us_layout_base_char(18), Some('1'));
        assert_eq!(us_layout_base_char(87), Some('5'));
        assert_eq!(us_layout_base_char(0xffff), None);
    }

    // ---- space (gpui names it "space", but it is a printable) ----------------

    #[test]
    fn build_space_is_space_char_not_s() {
        // gpui reports the space bar as key "space", key_char " ". The name must
        // map back to ' ', not 's' (chars().next() of "space").
        let plain = build_key_input(
            &ks("space", Some(" "), mods(false, false, false, false)),
            KeyEventType::Press,
            Some(49), // kVK_Space
            false,
        )
        .unwrap();
        assert_eq!(plain.key, Key::Char(' '));
        assert_eq!(plain.text.as_deref(), Some(" "));

        let ctrl = build_key_input(
            &ks("space", Some(" "), mods(false, false, true, false)),
            KeyEventType::Press,
            Some(49),
            false,
        )
        .unwrap();
        assert_eq!(ctrl.key, Key::Char(' '));
        assert!(ctrl.modifiers.ctrl);
    }

    #[test]
    fn ctrl_space_encodes_nul_legacy_and_csi_u_disambiguate() {
        // Ctrl+Space is the regression: it must be NUL (0x00), never 0x13 (XOFF,
        // which 's' would give). Legacy -> 0x00; under DISAMBIGUATE -> ESC[32;5u.
        let ctrl_space = build_key_input(
            &ks("space", Some(" "), mods(false, false, true, false)),
            KeyEventType::Press,
            Some(49),
            false,
        )
        .unwrap();
        assert_eq!(
            KeyEncoder::default().encode(&ctrl_space),
            Some(vec![0x00])
        );
        assert_eq!(
            KeyEncoder::new(KittyFlags::DISAMBIGUATE).encode(&ctrl_space),
            Some(b"\x1b[32;5u".to_vec())
        );
    }

    // ---- kitty_forwards_super ------------------------------------------------

    #[test]
    fn kitty_forwards_super_only_on_disambiguate_or_all_keys() {
        assert!(!kitty_forwards_super(TermMode::NONE));
        // The bits that make kitty "active" but do NOT lift ⌘ off the legacy path.
        assert!(!kitty_forwards_super(TermMode::REPORT_EVENT_TYPES));
        assert!(!kitty_forwards_super(TermMode::REPORT_ALTERNATE_KEYS));
        assert!(!kitty_forwards_super(TermMode::REPORT_ASSOCIATED_TEXT));
        // These do.
        assert!(kitty_forwards_super(TermMode::DISAMBIGUATE_ESC_CODES));
        assert!(kitty_forwards_super(TermMode::REPORT_ALL_KEYS_AS_ESC));
    }

    // ---- bare-modifier reports (flagsChanged path) ---------------------------

    #[test]
    fn modifier_named_key_splits_left_and_right() {
        assert_eq!(modifier_named_key(55), Some(NamedKey::SuperLeft));
        assert_eq!(modifier_named_key(54), Some(NamedKey::SuperRight));
        assert_eq!(modifier_named_key(56), Some(NamedKey::ShiftLeft));
        assert_eq!(modifier_named_key(60), Some(NamedKey::ShiftRight));
        assert_eq!(modifier_named_key(59), Some(NamedKey::ControlLeft));
        assert_eq!(modifier_named_key(62), Some(NamedKey::ControlRight));
        assert_eq!(modifier_named_key(58), Some(NamedKey::AltLeft));
        assert_eq!(modifier_named_key(61), Some(NamedKey::AltRight));
        // Not a modifier key (kVK_ANSI_A) / caps lock (kVK_CapsLock).
        assert_eq!(modifier_named_key(0), None);
        assert_eq!(modifier_named_key(57), None);
    }

    #[test]
    fn build_modifier_input_press_release_from_aggregate() {
        // Left-shift down: the new aggregate has shift set -> Press, ShiftLeft.
        let press =
            build_modifier_input(56, &mods(true, false, false, false), false).unwrap();
        assert_eq!(press.key, Key::Named(NamedKey::ShiftLeft));
        assert_eq!(press.event, KeyEventType::Press);
        assert!(press.modifiers.shift);
        // Left-shift up: aggregate shift cleared -> Release.
        let release =
            build_modifier_input(56, &mods(false, false, false, false), false).unwrap();
        assert_eq!(release.event, KeyEventType::Release);
        assert!(!release.modifiers.shift);

        // The full-kitty report matches the encoder's expectation (ESC[57441;2u
        // press / ESC[57441;1:3u release under REPORT_ALL_KEYS+REPORT_EVENT_TYPES).
        let enc = KeyEncoder::new(KittyFlags::REPORT_ALL_KEYS | KittyFlags::REPORT_EVENT_TYPES);
        assert_eq!(enc.encode(&press), Some(b"\x1b[57441;2u".to_vec()));
        assert_eq!(enc.encode(&release), Some(b"\x1b[57441;1:3u".to_vec()));
        // A non-modifier keyCode yields nothing to report.
        assert!(build_modifier_input(0, &mods(false, false, false, false), false).is_none());
    }
}
