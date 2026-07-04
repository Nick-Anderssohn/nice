//! nice-term-input — the pure, gpui-free input-encoding layer for Nice's
//! terminal view (R5, slice 1).
//!
//! Everything here is a pure function of plain key/modifier/mouse structs — no
//! gpui or AppKit types cross this boundary. The R5 event-edge slices translate
//! gpui `Keystroke`/`KeyDownEvent`/`ScrollWheelEvent` (plus the
//! `[NSApp currentEvent].keyCode` side-channel for layout-independent keys) into
//! these types, wire the platform `InputHandler`, and drive the IME state
//! machine; none of that lives here. Keeping the encoders in their own
//! gpui-free crate (per the crates/README layering rule, alongside
//! `nice-term-core` and `nice-theme`) means the byte-exact encoder tests build
//! and run without the gpui stack.
//!
//! ## Pieces
//!
//! * [`key`] — the shared plain input types ([`key::KeyInput`],
//!   [`key::Modifiers`], [`key::Key`] / [`key::NamedKey`]) the encoders consume.
//! * [`ime_state`] — the pure marked-text (preedit) state machine
//!   ([`ime_state::ImeState`]) the R5 InputHandler adapter is a thin shell over:
//!   the five G1 IME gating behaviours (compose/commit pty-silence, the
//!   Enter-swallow flag, the never-`None` candidate anchor) as unit-testable
//!   transitions, gpui-free like the encoders.
//! * [`keyboard`] — the keyboard encoder: kitty CSI-u with the full
//!   progressive-enhancement flag ladder ([`keyboard::KittyFlags`]), press /
//!   repeat / release, functional keys, modifiers-as-functional-keys, the
//!   Cmd-as-super `ESC[99;9u` path, plus the legacy VT fallback (raw text, C0
//!   control chars, `ESC[…` / `ESC O …` functional keys) when no kitty flag is
//!   active.
//! * [`mouse`] — the VT mouse-report encoder in the X10 / SGR / UTF-8
//!   coordinate encodings (SGR is the one that survives past column 223).
//! * [`paste`] — the bracketed-paste (DECSET 2004) wrap helper.
//! * [`config`] — the option-as-meta config value.
//!
//! ## Licensing
//!
//! The encoders are clean-room implementations guided only by the
//! license-permitted references named in the R5 plan's Ground rules: the
//! SwiftTerm fork (ours — the behavior Nice ships today) and alacritty's
//! `keyboard.rs` / mouse reporters (Apache-2.0). Zed's GPL-3.0 `crates/terminal`
//! and `crates/terminal_view` are never consulted.

pub mod config;
pub mod ime_state;
pub mod key;
pub mod keyboard;
pub mod mouse;
pub mod paste;

pub use config::{OptionAsMeta, OptionSide};
pub use ime_state::{byte_to_utf16, utf16_len, utf16_to_byte, CommitOutcome, ImeState};
pub use key::{Key, KeyEventType, KeyInput, KeyLocation, Modifiers, NamedKey};
pub use keyboard::{KeyEncoder, KittyFlags};
pub use mouse::{encode_mouse, MouseAction, MouseButton, MouseInput, MouseProtocol};
pub use paste::{wrap_bracketed_paste, PASTE_END, PASTE_START};
