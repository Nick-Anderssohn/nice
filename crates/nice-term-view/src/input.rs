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

use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vi_mode::ViMotion;
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

/// A scrollback navigation a keystroke drives on Nice's viewport instead of
/// encoding to the pty (Phase 0 keyboard scrollback — see
/// [`scrollback_key_action`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollbackAction {
    /// One screen toward history (Shift+PageUp).
    PageUp,
    /// One screen toward the bottom (Shift+PageDown).
    PageDown,
    /// Jump to the oldest scrollback line (Shift+Home).
    Top,
    /// Jump back to the live bottom (Shift+End).
    Bottom,
}

/// Which scrollback navigation a keystroke drives, if any (Phase 0 keyboard
/// scrollback; scrolling was wheel-only before). Shift+PageUp/PageDown page
/// through history and Shift+Home/End jump to the ends — the alacritty/Ghostty
/// convention; the PLAIN keys keep encoding (`\e[5~`…, which less/vim/etc.
/// depend on). Two gates:
///
/// * Shift must be the only chord modifier — a ctrl/alt/⌘-bearing chord stays
///   terminal input. (macOS sets the `function` flag on every navigation key
///   itself, so `function` deliberately does not disqualify.)
/// * Not on the alternate screen: a fullscreen TUI has no scrollback and owns
///   its keys, so there even the Shift variants encode.
pub fn scrollback_key_action(
    key: &str,
    m: gpui::Modifiers,
    mode: TermMode,
) -> Option<ScrollbackAction> {
    if !m.shift || m.control || m.alt || m.platform {
        return None;
    }
    if mode.contains(TermMode::ALT_SCREEN) {
        return None;
    }
    match key {
        "pageup" => Some(ScrollbackAction::PageUp),
        "pagedown" => Some(ScrollbackAction::PageDown),
        "home" => Some(ScrollbackAction::Top),
        "end" => Some(ScrollbackAction::Bottom),
        _ => None,
    }
}

// ---- Copy mode (Phase 3) ---------------------------------------------------
//
// Copy mode IS `TermMode::VI` (P1); this section is its *pure* half — the key
// table and the three gate predicates. Everything here is a total function over
// plain values so it can be unit-tested without a `TerminalView` (which needs a
// spawned session plus a gpui window); the wiring that consumes it lives in
// `view.rs`.

/// What a keystroke does while the pane is in copy mode — the result of
/// [`copy_mode_key_action`], performed by the view against the session handle's
/// copy-mode API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyModeAction {
    /// Move the vi cursor: `hjkl` + arrows, `w`/`b`/`e` (semantic) and
    /// `W`/`B`/`E` (whitespace), `0`/`$`/`^`, `H`/`M`/`L`, `%`, `{`/`}` (D3).
    Motion(ViMotion),
    /// `g` (and Shift+Home) — jump to the oldest line still in scrollback.
    Top,
    /// `G` (and Shift+End) — jump back to the newest output.
    Bottom,
    /// Page the viewport, dragging the vi cursor with it: `⌃u`/`⌃d` are the half
    /// pages, `⌃b`/`⌃f` and Shift+PageUp/PageDown the full ones.
    Page {
        /// Toward history (up) rather than toward the live bottom.
        toward_history: bool,
        /// Half a screen rather than a whole one.
        half: bool,
    },
    /// `v` / `V` / `⌃v` — toggle a selection of this kind at the vi cursor (P5:
    /// none ⇒ start, same kind ⇒ clear, different kind ⇒ rebuild).
    ToggleSelection(SelectionType),
    /// `y` and Enter — copy the selection to the clipboard and LEAVE copy mode
    /// (tmux's copy-and-cancel). With nothing selected this is a no-op that
    /// stays in the mode (P4).
    Yank,
    /// ⌘C — copy the selection and STAY, exactly like today's ⌘C (P4).
    YankStay,
    /// ⌘V — consumed and dropped: pasting into scrollback is meaningless (P4).
    SwallowPaste,
    /// `/` (forward) and `?` (backward) — ask the app for the search field (P2:
    /// the field lives in the app crate, so the view emits
    /// [`TerminalEvent::SearchRequested`] instead of opening it).
    ///
    /// [`TerminalEvent::SearchRequested`]: crate::TerminalEvent::SearchRequested
    OpenSearch {
        /// Search history-ward (`?`) rather than toward the live bottom (`/`).
        backward: bool,
    },
    /// `n` — the next match in the confirmed search direction.
    NextMatch,
    /// `N` — the next match *against* the confirmed direction (P7).
    PrevMatch,
    /// Esc and `q` — leave copy mode (P6: selection, search and viewport reset).
    Exit,
    /// Consumed, doing nothing. The leak-proof default: while VI is on, EVERY
    /// key is swallowed (P4), so an unbound key can never reach the encoder and
    /// type into the shell behind the user's back.
    Swallow,
}

/// The copy-mode key table (D3) — the whole of it, in one pure match.
///
/// **Total by design.** It never declines: the default arm is
/// [`CopyModeAction::Swallow`], which is P4's guarantee that nothing leaks to
/// the pty while VI is on. (The plan sketched an `Option` return; an
/// always-`Some` option would only invite an `unwrap_or` at the call site,
/// which is the leak this table exists to prevent.)
///
/// Modifier rungs, in order:
///
/// * ⌘ — only ⌘C (copy-and-stay) and ⌘V (swallowed) mean anything.
/// * ⌃ — the paging chords and `⌃v` block selection.
/// * ⌥ or a mixed rung — swallowed; Nice's own ⌃⌘ chords never arrive here at
///   all (gpui matches actions BEFORE view key listeners).
/// * bare / Shift — the motions and verbs.
///
/// Shift is folded two different ways by gpui's macOS backend, and both are
/// handled: a shifted **letter** arrives as the lowercase key plus `shift`
/// (`W` ⇒ `"w"` + shift), while shifted **punctuation** arrives as the shifted
/// character with the flag cleared (`$` ⇒ `"$"`, no shift) — so the punctuation
/// rows ignore `shift` entirely, and `/` accepts either spelling of `?`.
///
/// The Shift+PageUp/PageDown/Home/End rows mirror [`scrollback_key_action`] on
/// purpose (I4): those keys normally act inside `dispatch_key`, which the
/// copy-mode gate never reaches, so without them today's scrollback keys would
/// go dead exactly while the user is navigating scrollback.
pub fn copy_mode_key_action(key: &str, m: gpui::Modifiers) -> CopyModeAction {
    use CopyModeAction as A;
    use SelectionType as S;
    use ViMotion as V;

    // ⌘ rung: the two editing chords, everything else dropped.
    if m.platform {
        return if m.control || m.alt {
            A::Swallow
        } else {
            match key {
                "c" => A::YankStay,
                "v" => A::SwallowPaste,
                _ => A::Swallow,
            }
        };
    }

    // ⌃ rung: paging + block selection.
    if m.control {
        return if m.alt {
            A::Swallow
        } else {
            match key {
                "u" => A::Page { toward_history: true, half: true },
                "d" => A::Page { toward_history: false, half: true },
                "b" => A::Page { toward_history: true, half: false },
                "f" => A::Page { toward_history: false, half: false },
                "v" => A::ToggleSelection(S::Block),
                _ => A::Swallow,
            }
        };
    }

    // ⌥ is an input modifier (Meta / dead keys), never a copy-mode rung.
    if m.alt {
        return A::Swallow;
    }

    // Shift-folded punctuation: the flag is already spent on the character.
    match key {
        "$" => return A::Motion(V::Last),
        "^" => return A::Motion(V::FirstOccupied),
        "%" => return A::Motion(V::Bracket),
        "{" => return A::Motion(V::ParagraphUp),
        "}" => return A::Motion(V::ParagraphDown),
        "?" => return A::OpenSearch { backward: true },
        // A layout that keeps the flag instead of folding it still gets `?`.
        "/" => return A::OpenSearch { backward: m.shift },
        _ => {}
    }

    if m.shift {
        return match key {
            "h" => A::Motion(V::High),
            "m" => A::Motion(V::Middle),
            "l" => A::Motion(V::Low),
            "w" => A::Motion(V::WordRight),
            "b" => A::Motion(V::WordLeft),
            "e" => A::Motion(V::WordRightEnd),
            "g" => A::Bottom,
            "n" => A::PrevMatch,
            "v" => A::ToggleSelection(S::Lines),
            // I4: today's keyboard scrollback, kept alive inside the mode.
            "pageup" => A::Page { toward_history: true, half: false },
            "pagedown" => A::Page { toward_history: false, half: false },
            "home" => A::Top,
            "end" => A::Bottom,
            _ => A::Swallow,
        };
    }

    match key {
        "h" | "left" => A::Motion(V::Left),
        "j" | "down" => A::Motion(V::Down),
        "k" | "up" => A::Motion(V::Up),
        "l" | "right" => A::Motion(V::Right),
        "0" => A::Motion(V::First),
        // vim's distinction, which alacritty models the same way: lowercase is
        // the semantic word, uppercase the whitespace-separated WORD.
        "w" => A::Motion(V::SemanticRight),
        "b" => A::Motion(V::SemanticLeft),
        "e" => A::Motion(V::SemanticRightEnd),
        "g" => A::Top,
        "n" => A::NextMatch,
        "v" => A::ToggleSelection(S::Simple),
        "y" | "enter" => A::Yank,
        "escape" | "q" => A::Exit,
        _ => A::Swallow,
    }
}

/// Which gate in `TerminalView::on_key_down` owns a key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyGate {
    /// Copy mode: the key runs through [`copy_mode_key_action`] and is consumed.
    CopyMode,
    /// A held (dead-child) pane: only ⌘C, keyboard scrollback and the dismiss
    /// Enter survive; everything else is consumed inert.
    Held,
    /// The ordinary path — IME gates, ⌘V/⌘C, then the encoder.
    Encode,
}

/// Which gate claims a key press, i.e. the ORDER of the two early gates (P10).
///
/// Copy mode wins over the held gate: keyboard-selecting what a finished process
/// printed is a held pane's whole remaining purpose, so copy mode has to work on
/// a dead pane's output. The held gate's own dismiss-Enter applies once VI is
/// off — in the mode, Enter means yank-and-exit.
pub fn key_gate(copy_mode: bool, held: bool) -> KeyGate {
    if copy_mode {
        KeyGate::CopyMode
    } else if held {
        KeyGate::Held
    } else {
        KeyGate::Encode
    }
}

/// The three platform IME callbacks the `TermInputHandler` drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImeCallback {
    /// `setMarkedText:` — a composition started or changed.
    SetMarked,
    /// `insertText:` — a composition (or a plain printable) committed.
    Commit,
    /// `unmarkText` — the pending composition is accepted as typed.
    Unmark,
}

/// What an IME callback is allowed to do (see [`ime_gate`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImeGate {
    /// Run the [`ImeState`] transition at all.
    ///
    /// [`ImeState`]: nice_term_input::ImeState
    pub run_transition: bool,
    /// Snap a scrolled-up viewport back to the live bottom.
    pub snap_to_bottom: bool,
    /// Write the callback's text to the pty.
    pub write_pty: bool,
}

/// What an IME callback may do, given whether the pane is in copy mode (P4's
/// third gate).
///
/// Dead keys and in-flight compositions reach the platform input handler WITHOUT
/// passing any key listener, so the `on_key_down` table alone cannot keep the
/// pty silent. In copy mode these callbacks drop the pty write and the
/// snap-to-bottom — but `Commit` and `Unmark` still run their state transition
/// with the output discarded. That distinction is load-bearing: a bare early
/// return would leave marked state `Some`, gpui would keep routing every key
/// through the input context, and after the mode exits the composing gate would
/// eat every keystroke — a keyboard-dead pane. Running the transition means a
/// composition in flight when `⌃⌘c` fires clears itself at commit time.
///
/// `SetMarked` is the one safe plain skip: Nice never learns of the composition,
/// so `is_composing` never arms in the first place.
pub fn ime_gate(callback: ImeCallback, copy_mode: bool) -> ImeGate {
    match (callback, copy_mode) {
        (ImeCallback::SetMarked, false) => ImeGate {
            run_transition: true,
            snap_to_bottom: true,
            write_pty: false, // marking never writes; the commit does
        },
        (ImeCallback::SetMarked, true) => ImeGate {
            run_transition: false,
            snap_to_bottom: false,
            write_pty: false,
        },
        (ImeCallback::Commit, copy) => ImeGate {
            run_transition: true,
            snap_to_bottom: !copy,
            write_pty: !copy,
        },
        (ImeCallback::Unmark, copy) => ImeGate {
            run_transition: true,
            snap_to_bottom: false, // unmark never snapped
            write_pty: !copy,
        },
    }
}

/// Whether a mouse event belongs to the **app** (a VT report to the pty) rather
/// than to Nice's local handling — the predicate at all four mouse gates (P10).
///
/// Copy mode acts exactly like the existing Shift override: a mouse-mode TUI
/// owns the mouse normally, but while VI is on the wheel scrolls the viewport,
/// a click moves the vi cursor and a drag selects, with nothing reaching the
/// running app. tmux captures the mouse in copy mode the same way.
pub fn mouse_reports_to_app(mode: TermMode, shift: bool, copy_mode: bool) -> bool {
    crate::mouse::reporting_active(mode) && !shift && !copy_mode
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

    // MARK: - scrollback_key_action (Phase 0 keyboard scrollback)

    #[test]
    fn shift_nav_keys_scroll_on_the_primary_screen() {
        let shift = mods(true, false, false, false);
        assert_eq!(
            scrollback_key_action("pageup", shift, TermMode::NONE),
            Some(ScrollbackAction::PageUp)
        );
        assert_eq!(
            scrollback_key_action("pagedown", shift, TermMode::NONE),
            Some(ScrollbackAction::PageDown)
        );
        assert_eq!(
            scrollback_key_action("home", shift, TermMode::NONE),
            Some(ScrollbackAction::Top)
        );
        assert_eq!(
            scrollback_key_action("end", shift, TermMode::NONE),
            Some(ScrollbackAction::Bottom)
        );
        // macOS sets `function` on navigation keys itself — must not disqualify.
        let mut shift_fn = shift;
        shift_fn.function = true;
        assert_eq!(
            scrollback_key_action("pageup", shift_fn, TermMode::NONE),
            Some(ScrollbackAction::PageUp)
        );
    }

    #[test]
    fn plain_and_multi_modifier_nav_keys_stay_terminal_input() {
        // Plain PageUp keeps encoding (\e[5~ — less/vim depend on it).
        assert_eq!(scrollback_key_action("pageup", mods(false, false, false, false), TermMode::NONE), None);
        // Any additional chord modifier belongs to the terminal/app.
        assert_eq!(scrollback_key_action("pageup", mods(true, false, true, false), TermMode::NONE), None);
        assert_eq!(scrollback_key_action("home", mods(true, true, false, false), TermMode::NONE), None);
        assert_eq!(scrollback_key_action("end", mods(true, false, false, true), TermMode::NONE), None);
        // Non-navigation keys never scroll.
        assert_eq!(scrollback_key_action("a", mods(true, false, false, false), TermMode::NONE), None);
    }

    #[test]
    fn alt_screen_declines_so_the_tui_gets_the_keys() {
        // A fullscreen TUI (vim, less) has no scrollback: even the Shift
        // variants encode to the app there.
        let shift = mods(true, false, false, false);
        assert_eq!(scrollback_key_action("pageup", shift, TermMode::ALT_SCREEN), None);
        assert_eq!(scrollback_key_action("end", shift, TermMode::ALT_SCREEN), None);
        // Kitty flags WITHOUT the alt screen still scroll (Claude Code's
        // inline TUI keeps normal-screen scrollback).
        assert_eq!(
            scrollback_key_action("pageup", shift, TermMode::DISAMBIGUATE_ESC_CODES),
            Some(ScrollbackAction::PageUp)
        );
    }

    // MARK: - copy_mode_key_action (Phase 3 copy mode, D3)

    /// A bare key, the way gpui's macOS backend reports one.
    fn bare(key: &str) -> CopyModeAction {
        copy_mode_key_action(key, mods(false, false, false, false))
    }

    /// A shifted LETTER: gpui keeps the flag and lowercases the key.
    fn shifted(key: &str) -> CopyModeAction {
        copy_mode_key_action(key, mods(true, false, false, false))
    }

    fn ctrl(key: &str) -> CopyModeAction {
        copy_mode_key_action(key, mods(false, false, true, false))
    }

    fn cmd(key: &str) -> CopyModeAction {
        copy_mode_key_action(key, mods(false, false, false, true))
    }

    #[test]
    fn copy_mode_motions_cover_the_d3_set() {
        use CopyModeAction::Motion;
        // hjkl and the arrows are the same motion.
        assert_eq!(bare("h"), Motion(ViMotion::Left));
        assert_eq!(bare("left"), Motion(ViMotion::Left));
        assert_eq!(bare("j"), Motion(ViMotion::Down));
        assert_eq!(bare("down"), Motion(ViMotion::Down));
        assert_eq!(bare("k"), Motion(ViMotion::Up));
        assert_eq!(bare("up"), Motion(ViMotion::Up));
        assert_eq!(bare("l"), Motion(ViMotion::Right));
        assert_eq!(bare("right"), Motion(ViMotion::Right));

        // Line ends. `$` and `^` arrive shift-folded into the character.
        assert_eq!(bare("0"), Motion(ViMotion::First));
        assert_eq!(bare("$"), Motion(ViMotion::Last));
        assert_eq!(bare("^"), Motion(ViMotion::FirstOccupied));

        // vim's word-vs-WORD split, which alacritty models as semantic-vs-word.
        assert_eq!(bare("w"), Motion(ViMotion::SemanticRight));
        assert_eq!(bare("b"), Motion(ViMotion::SemanticLeft));
        assert_eq!(bare("e"), Motion(ViMotion::SemanticRightEnd));
        assert_eq!(shifted("w"), Motion(ViMotion::WordRight));
        assert_eq!(shifted("b"), Motion(ViMotion::WordLeft));
        assert_eq!(shifted("e"), Motion(ViMotion::WordRightEnd));

        // Screen thirds.
        assert_eq!(shifted("h"), Motion(ViMotion::High));
        assert_eq!(shifted("m"), Motion(ViMotion::Middle));
        assert_eq!(shifted("l"), Motion(ViMotion::Low));

        // Brackets and paragraphs (all shift-folded punctuation).
        assert_eq!(bare("%"), Motion(ViMotion::Bracket));
        assert_eq!(bare("{"), Motion(ViMotion::ParagraphUp));
        assert_eq!(bare("}"), Motion(ViMotion::ParagraphDown));
    }

    #[test]
    fn copy_mode_jumps_and_paging() {
        assert_eq!(bare("g"), CopyModeAction::Top);
        assert_eq!(shifted("g"), CopyModeAction::Bottom);
        assert_eq!(
            ctrl("u"),
            CopyModeAction::Page { toward_history: true, half: true }
        );
        assert_eq!(
            ctrl("d"),
            CopyModeAction::Page { toward_history: false, half: true }
        );
        assert_eq!(
            ctrl("b"),
            CopyModeAction::Page { toward_history: true, half: false }
        );
        assert_eq!(
            ctrl("f"),
            CopyModeAction::Page { toward_history: false, half: false }
        );
    }

    #[test]
    fn copy_mode_keeps_todays_scrollback_keys_alive() {
        // I4: these normally act inside `dispatch_key`, which the copy-mode gate
        // never reaches — without these rows they would go dead exactly while
        // the user is navigating scrollback.
        assert_eq!(
            shifted("pageup"),
            CopyModeAction::Page { toward_history: true, half: false }
        );
        assert_eq!(
            shifted("pagedown"),
            CopyModeAction::Page { toward_history: false, half: false }
        );
        assert_eq!(shifted("home"), CopyModeAction::Top);
        assert_eq!(shifted("end"), CopyModeAction::Bottom);
        // macOS sets `function` on navigation keys itself — must not disqualify.
        let mut shift_fn = mods(true, false, false, false);
        shift_fn.function = true;
        assert_eq!(
            copy_mode_key_action("pageup", shift_fn),
            CopyModeAction::Page { toward_history: true, half: false }
        );
        // The plain variants are swallowed like every other unbound key: in copy
        // mode nothing reaches the pty.
        assert_eq!(bare("pageup"), CopyModeAction::Swallow);
        assert_eq!(bare("home"), CopyModeAction::Swallow);
    }

    #[test]
    fn copy_mode_selection_verbs_and_yank() {
        assert_eq!(bare("v"), CopyModeAction::ToggleSelection(SelectionType::Simple));
        assert_eq!(shifted("v"), CopyModeAction::ToggleSelection(SelectionType::Lines));
        assert_eq!(ctrl("v"), CopyModeAction::ToggleSelection(SelectionType::Block));
        // `y` and Enter copy-and-exit; ⌘C copies and stays; ⌘V is swallowed (P4).
        assert_eq!(bare("y"), CopyModeAction::Yank);
        assert_eq!(bare("enter"), CopyModeAction::Yank);
        assert_eq!(cmd("c"), CopyModeAction::YankStay);
        assert_eq!(cmd("v"), CopyModeAction::SwallowPaste);
    }

    #[test]
    fn copy_mode_search_verbs() {
        assert_eq!(bare("/"), CopyModeAction::OpenSearch { backward: false });
        // `?` arrives shift-folded on macOS; a layout that keeps the flag is
        // accepted too.
        assert_eq!(bare("?"), CopyModeAction::OpenSearch { backward: true });
        assert_eq!(shifted("/"), CopyModeAction::OpenSearch { backward: true });
        assert_eq!(bare("n"), CopyModeAction::NextMatch);
        assert_eq!(shifted("n"), CopyModeAction::PrevMatch);
    }

    #[test]
    fn copy_mode_exit_keys() {
        assert_eq!(bare("escape"), CopyModeAction::Exit);
        assert_eq!(bare("q"), CopyModeAction::Exit);
    }

    #[test]
    fn copy_mode_default_arm_swallows_everything_else() {
        // The P4 guarantee: no unbound key can reach the encoder while VI is on.
        for key in [
            "a", "c", "i", "p", "r", "s", "t", "x", "z", "1", "space", "tab", "backspace",
            "delete", "f5", "insert", ";", ",", ".",
        ] {
            assert_eq!(bare(key), CopyModeAction::Swallow, "bare {key}");
        }
        // Bound letters on the WRONG rung are swallowed, not misread.
        assert_eq!(cmd("h"), CopyModeAction::Swallow);
        assert_eq!(ctrl("h"), CopyModeAction::Swallow);
        assert_eq!(cmd("y"), CopyModeAction::Swallow);
        assert_eq!(ctrl("n"), CopyModeAction::Swallow);
        // ⌥ is an input modifier (Meta / dead keys), never a copy-mode rung.
        assert_eq!(
            copy_mode_key_action("h", mods(false, true, false, false)),
            CopyModeAction::Swallow
        );
        // Mixed rungs (⌃⌘, ⌥⌘, ⌃⌥) belong to the app's chords, which are matched
        // as gpui actions long before this table sees them.
        assert_eq!(
            copy_mode_key_action("c", mods(false, false, true, true)),
            CopyModeAction::Swallow
        );
        assert_eq!(
            copy_mode_key_action("v", mods(false, true, false, true)),
            CopyModeAction::Swallow
        );
        assert_eq!(
            copy_mode_key_action("u", mods(false, true, true, false)),
            CopyModeAction::Swallow
        );
    }

    // MARK: - the three gate predicates (P4 / P10)

    #[test]
    fn copy_mode_gate_runs_before_the_held_gate() {
        // P10: copy mode must work on a dead pane's output, so it wins over the
        // held gate — the held gate's dismiss-Enter applies once VI is off.
        assert_eq!(key_gate(true, true), KeyGate::CopyMode);
        assert_eq!(key_gate(true, false), KeyGate::CopyMode);
        assert_eq!(key_gate(false, true), KeyGate::Held);
        assert_eq!(key_gate(false, false), KeyGate::Encode);
    }

    #[test]
    fn ime_gate_keeps_the_pty_silent_but_still_transitions() {
        // Marking is a plain skip: Nice never learns of the composition.
        let marked = ime_gate(ImeCallback::SetMarked, true);
        assert!(!marked.run_transition);
        assert!(!marked.snap_to_bottom);
        assert!(!marked.write_pty);

        // Commit and unmark still run their transition with the output
        // discarded — a bare early return would strand marked state `Some` and
        // leave the pane keyboard-dead after the mode exits (B1/F3).
        let commit = ime_gate(ImeCallback::Commit, true);
        assert!(commit.run_transition);
        assert!(!commit.snap_to_bottom);
        assert!(!commit.write_pty);

        let unmark = ime_gate(ImeCallback::Unmark, true);
        assert!(unmark.run_transition);
        assert!(!unmark.write_pty);
    }

    #[test]
    fn ime_gate_is_todays_behaviour_outside_copy_mode() {
        let marked = ime_gate(ImeCallback::SetMarked, false);
        assert!(marked.run_transition);
        assert!(marked.snap_to_bottom);
        assert!(!marked.write_pty); // marking never writes; the commit does

        let commit = ime_gate(ImeCallback::Commit, false);
        assert!(commit.run_transition);
        assert!(commit.snap_to_bottom);
        assert!(commit.write_pty);

        let unmark = ime_gate(ImeCallback::Unmark, false);
        assert!(unmark.run_transition);
        assert!(!unmark.snap_to_bottom); // unmark never snapped
        assert!(unmark.write_pty);
    }

    #[test]
    fn mouse_reports_suspend_in_copy_mode_like_shift() {
        let reporting = TermMode::MOUSE_REPORT_CLICK;
        // Normal: the app owns the mouse.
        assert!(mouse_reports_to_app(reporting, false, false));
        // Shift is the existing local override; copy mode is the new one, and
        // either alone suspends reporting.
        assert!(!mouse_reports_to_app(reporting, true, false));
        assert!(!mouse_reports_to_app(reporting, false, true));
        assert!(!mouse_reports_to_app(reporting, true, true));
        // A pane that never asked for mouse reports is unaffected either way.
        assert!(!mouse_reports_to_app(TermMode::NONE, false, false));
        assert!(!mouse_reports_to_app(TermMode::NONE, false, true));
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
