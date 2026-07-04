//! `TerminalView` — the gpui view that owns a [`FocusHandle`] and paints a
//! [`TerminalSessionHandle`] through a [`TerminalElement`] each frame.
//!
//! It observes the session handle (repaint on the handle's `notify`) and owns a
//! `FocusHandle` (needed for R5 key dispatch + DECSET-1004 focus reporting).
//! The caret's solid/hollow state is **computed** from
//! `focus_handle.is_focused(window) && window.is_window_active()` every frame —
//! there is deliberately **no separately-maintained focus flag** (that is
//! pain-catalog mechanism #5, remembered-not-computed state). R13 later directs
//! focus here via `focus_handle.focus()`.
//!
//! ## R5 input path
//!
//! The view is the terminal's input edge. It owns the pure [`ImeState`]
//! marked-text machine and drives it through the platform [`TermInputHandler`]
//! (registered every frame in the element's paint). Three gpui listeners feed the
//! keyboard encoder:
//!
//! * [`on_key_down`](TerminalView::on_key_down) / [`on_key_up`](TerminalView::on_key_up)
//!   translate gpui `Keystroke`s (plus the injected macOS keyCode side-channel)
//!   into `nice-term-input` [`KeyInput`]s and write the encoded bytes to the pty
//!   — but **only** for keys the terminal owns (functional keys, ctrl/⌘/Meta
//!   chords, and — in full kitty mode — every key). Plain and shift printables
//!   fall through to the platform IME `insertText` path (so CJK composition and
//!   dead keys work); their committed text is written by
//!   [`ime_commit`](TerminalView::ime_commit) as data, never through the encoder.
//! * [`on_modifiers_changed`](TerminalView::on_modifiers_changed) is the kitty
//!   modifiers-as-functional-keys report (bare Shift/Ctrl/Alt/⌘ press+release):
//!   active only under REPORT_ALL_KEYS, resolving the left/right key from the
//!   flagsChanged keyCode side-channel.
//! * The five G1 IME gating behaviours live in [`ImeState`]; this view is the
//!   thin adapter (marked-text updates, the Enter-commit swallow, the
//!   never-`None` candidate anchor at the grid cursor cell).
//!
//! ## R5 mouse / paste / copy / focus (slice 3)
//!
//! The remaining VT input surface is wired here too, on top of the same handle:
//!
//! * **VT mouse reporting** — when the app requests it (the core `Term`'s
//!   `MOUSE_MODE` bits), `on_mouse_*` hit-test the pixel position to a cell
//!   ([`mouse::cell_from_offset`], reusing R4's [`grid_top_y`] metrics) and
//!   encode X10/SGR/UTF-8 reports through slice-1's
//!   [`encode_mouse`](nice_term_input::encode_mouse). A held **Shift** is the
//!   local override: it forces selection/scroll even while the app reports.
//! * **Local drag selection** — a bare left drag (or any drag with Shift) drives
//!   R4's [`TerminalSessionHandle::set_selection`] in buffer coordinates.
//! * **⌘V paste** — the clipboard text is framed by
//!   [`wrap_bracketed_paste`](nice_term_input::wrap_bracketed_paste) gated on the
//!   core's `bracketed_paste_active()`, then written to the pty.
//! * **⌘C copy** — a live selection is rendered to a string and written to the
//!   pasteboard (only while kitty is off; under kitty ⌘C forwards `ESC[99;9u`).
//! * **DECSET-1004 focus in/out** — a change in the combined focus predicate
//!   (`is_focused && window active`, the same value the caret uses) emits
//!   `ESC[I` / `ESC[O` when the app enabled focus reporting.
//!
//! [`KeyInput`]: nice_term_input::KeyInput

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::TermMode;
use gpui::{
    div, point, prelude::*, px, size, App, Bounds, ClipboardItem, Context, Entity, FocusHandle,
    Focusable, Font, FontFeatures, FontStyle, FontWeight, KeyDownEvent, KeyUpEvent, Keystroke,
    ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, Render, ScrollWheelEvent, SharedString, Subscription, TextRun, Window,
};

use nice_term_input::{
    encode_mouse, utf16_to_byte, wrap_bracketed_paste, ImeState, KeyEventType, MouseAction,
    MouseButton as VtButton, MouseInput, OptionAsMeta, OptionSide,
};
use nice_theme::Srgba;

use crate::element::{grid_top_y, ImeInput, TerminalElement, TerminalMetrics};
use crate::input::{
    build_key_input, build_modifier_input, encoder_config, kitty_forwards_super, named_key_for,
    KeyCodeProbe,
};
use crate::mouse;
use crate::session_handle::TerminalSessionHandle;
use crate::theme::TerminalTheme;

/// A view over one terminal session. Construct with [`TerminalView::new`] from a
/// session handle + theme value + accent (R2) + cell metrics.
pub struct TerminalView {
    handle: Entity<TerminalSessionHandle>,
    theme: TerminalTheme,
    accent: Srgba,
    font_family: SharedString,
    font_px: f32,
    metrics: TerminalMetrics,
    focus_handle: FocusHandle,
    /// The pure marked-text (preedit) state machine driven by the platform IME.
    ime: ImeState,
    /// Option-as-Meta policy (SwiftTerm-parity default `Both`). Consulted per
    /// keystroke to decide whether a ⌥-modified printable is a Meta chord (ESC
    /// prefix, bypasses the IME) or is left for the OS to compose.
    option_as_meta: OptionAsMeta,
    /// The injected macOS keyCode side-channel (built in `crates/nice/src/platform`).
    /// `None` until the app wires it; the encoder then falls back to gpui's key
    /// names alone (no layout-independent alternate-key recovery).
    keycode_probe: Option<KeyCodeProbe>,
    /// This frame's grid bounds, published by the element during paint and read
    /// by the mouse handlers on the next event for pixel→cell hit-testing. Shared
    /// so paint writes it without re-entering this entity (see [`TerminalElement`]).
    paint_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// An in-progress local selection drag, anchored at the **buffer** cell
    /// `(line, column)` the left button went down on (`line` negative in
    /// scrollback). `Some` between mouse-down and the ending mouse-up; each drag
    /// move rewrites the selection from this anchor to the current cell.
    drag_anchor: Option<(i32, usize)>,
    /// The last cell a mouse **report** was emitted for, to de-duplicate motion
    /// reports (an app gets one report per cell crossed, not per pixel moved).
    last_report_cell: Option<(usize, usize)>,
    /// Fractional wheel travel not yet emitted as a wheel report, in cells. Whole
    /// steps become button-64/65 reports; the remainder is kept so a slow
    /// trackpad still eventually reports (mirrors the scrollback accumulator).
    wheel_accum: f32,
    /// The last combined focus state (`is_focused && window active`) reported via
    /// DECSET-1004. `None` until the first render seeds it (so startup never emits
    /// a spurious `ESC[I`); thereafter a change edge-triggers a focus report.
    last_focus_reported: Option<bool>,
    /// Repaint subscription to the session handle. Held so it stays live for the
    /// view's lifetime.
    _handle_sub: Subscription,
}

/// A hit-tested grid cell: viewport coordinates (what a VT report carries) plus
/// the buffer line (what [`TerminalSessionHandle::set_selection`] wants —
/// negative in scrollback).
#[derive(Clone, Copy)]
struct Hit {
    col: usize,
    vrow: usize,
    buffer_line: i32,
}

/// Cap on wheel reports emitted for a single scroll event, so a hard trackpad
/// fling under app mouse reporting can't flood the pty with button-64/65 reports.
const WHEEL_REPORT_MAX: i32 = 8;

impl TerminalView {
    /// Build a view over `handle`, painting with `theme` (caret in `accent`
    /// unless the theme overrides the cursor) at `font_family` / `font_px` and
    /// the given cell `metrics`.
    pub fn new(
        handle: Entity<TerminalSessionHandle>,
        theme: TerminalTheme,
        accent: Srgba,
        font_family: SharedString,
        font_px: f32,
        metrics: TerminalMetrics,
        cx: &mut Context<Self>,
    ) -> Self {
        // Repaint whenever the session handle notifies (new output / events).
        let sub = cx.observe(&handle, |_this, _handle, cx| cx.notify());
        Self {
            handle,
            theme,
            accent,
            font_family,
            font_px,
            metrics,
            focus_handle: cx.focus_handle(),
            ime: ImeState::new(),
            option_as_meta: OptionAsMeta::default(),
            keycode_probe: None,
            paint_bounds: Rc::new(Cell::new(None)),
            drag_anchor: None,
            last_report_cell: None,
            wheel_accum: 0.0,
            last_focus_reported: None,
            _handle_sub: sub,
        }
    }

    /// The view's focus handle (R5 drives key input through it; R13 focuses it).
    pub fn focus_handle_ref(&self) -> &FocusHandle {
        &self.focus_handle
    }

    /// Install the macOS keyCode side-channel (see [`KeyCodeProbe`]). The app
    /// calls this once with a closure that reads `[NSApp currentEvent].keyCode`
    /// (built in `crates/nice/src/platform` — the sole objc2 home). Without it
    /// the encoder still works from gpui's key names; it just cannot recover the
    /// layout-independent base-layout codepoint for kitty alternate reporting.
    pub fn set_keycode_probe(&mut self, probe: KeyCodeProbe) {
        self.keycode_probe = Some(probe);
    }

    /// Set the Option-as-Meta policy (the R5 config surface; the settings UI is a
    /// later cycle). Defaults to [`OptionAsMeta::Both`] (SwiftTerm parity).
    pub fn set_option_as_meta(&mut self, policy: OptionAsMeta) {
        self.option_as_meta = policy;
    }

    // -- key input -------------------------------------------------------------

    /// The terminal's currently-tracked VT mode (kitty flags + DECCKM), read
    /// under a brief `Term` lock. `NONE` before the session is spawned.
    fn current_mode(&self, cx: &App) -> TermMode {
        self.handle
            .read(cx)
            .term()
            // `Term::mode()` returns `&TermMode`; copy it out (TermMode is `Copy`)
            // before the brief lock guard drops.
            .map(|term_arc| *term_arc.lock().mode())
            .unwrap_or(TermMode::NONE)
    }

    /// Write raw bytes to the child. Best-effort: a not-yet-spawned session
    /// errors, which is dropped (there is nowhere to send the keystroke yet).
    fn write_pty(&self, bytes: &[u8], cx: &App) {
        if !bytes.is_empty() {
            let _ = self.handle.read(cx).session().write_input(bytes);
        }
    }

    /// gpui key-down: the terminal's typed-input entry point.
    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let m = keystroke.modifiers;

        // Read+clear the Enter-swallow at the START of every key cycle — only an
        // Enter/Tab re-dispatched in the SAME native cycle as a composition commit
        // (the `doCommandBySelector(insertNewline:)` path) observes `true`.
        let swallow = self.ime.take_commit_swallow();

        // (G1 item 2) An Enter/Tab that just confirmed a composition this cycle is
        // swallowed — no CR/HT reaches the pty (the commit already wrote the text).
        let commit_confirm_key =
            (keystroke.key == "enter" || keystroke.key == "tab") && !m.control && !m.platform;
        if swallow && commit_confirm_key {
            cx.stop_propagation();
            cx.notify();
            return;
        }

        // (G1 items 1 & 3) While composing, all key handling belongs to the IME
        // (preedit edits, candidate navigation, commit): the pty stays silent.
        // gpui routes keys to the IME because `marked_text_range` is `Some`, so a
        // key that still lands here must not encode anything.
        if self.ime.is_composing() {
            cx.notify();
            return;
        }

        // ⌘V paste / ⌘C copy — the macOS-standard editing shortcuts, handled
        // before the key encoder. ⌘V always pastes (bracketed-wrapped per the
        // core's DECSET-2004 state); ⌘C copies a live selection UNLESS the encoder
        // would actually forward ⌘C as `ESC[99;9u` — i.e. `kitty_forwards_super`
        // (DISAMBIGUATE / REPORT_ALL_KEYS). Gating on plain `kitty_active` would
        // strand ⌘C under e.g. REPORT_EVENT_TYPES-only: copy skipped AND the
        // encoder emitting nothing. (Under real kitty, the "⌘C doesn't copy" quirk
        // is Claude-side and deliberately not fixed here.)
        if m.platform && !m.control && !m.alt {
            if keystroke.key == "v" {
                self.paste_clipboard(cx);
                cx.stop_propagation();
                cx.notify();
                return;
            }
            if keystroke.key == "c"
                && !kitty_forwards_super(self.current_mode(cx))
                && self.copy_selection(cx)
            {
                cx.stop_propagation();
                return;
            }
        }

        let event_type = if event.is_held {
            KeyEventType::Repeat
        } else {
            KeyEventType::Press
        };
        self.dispatch_key(keystroke, event_type, cx);
    }

    /// gpui key-up: only relevant to the kitty event-type ladder (press/repeat/
    /// release). In legacy and plain-kitty modes releases encode to nothing.
    fn on_key_up(&mut self, event: &KeyUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.ime.is_composing() {
            return;
        }
        let mode = self.current_mode(cx);
        if !mode.contains(TermMode::REPORT_EVENT_TYPES) {
            return; // the app did not ask for release reporting
        }
        let keycode = self.keycode_probe.as_ref().and_then(|probe| probe());
        let Some(input) = build_key_input(&event.keystroke, KeyEventType::Release, keycode, false)
        else {
            return;
        };
        if let Some(bytes) = encoder_config(mode, false).encode(&input) {
            self.write_pty(&bytes, cx);
        }
    }

    /// Decide whether a (non-composing) keystroke is terminal-owned — encode it
    /// and write the bytes, consuming the event — or should fall through to the
    /// platform IME / app keybindings.
    fn dispatch_key(&mut self, keystroke: &Keystroke, event: KeyEventType, cx: &mut Context<Self>) {
        let mode = self.current_mode(cx);
        let m = keystroke.modifiers;

        // ⌥-as-Meta: gpui does not report which Option key is held, so the
        // per-side policy is resolved best-effort (Both/Off are side-independent;
        // Left/RightOnly assume the left key — a settings-UI-era refinement).
        let alt_meta = m.alt && self.option_as_meta.treats_as_meta(OptionSide::Left);

        let named = named_key_for(&keystroke.key).is_some();
        let should_encode = if named {
            // Functional keys (arrows, F-keys, Enter/Tab/Backspace/…) never reach
            // the IME — always terminal input.
            true
        } else {
            let report_all = mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC);
            (m.control && !m.platform)   // ctrl chords are terminal input in every mode
                || (m.platform && kitty_forwards_super(mode)) // ⌘ only when it'd be CSI-u-forwarded
                || alt_meta               // ⌥-as-Meta prefixes ESC and bypasses the IME
                || report_all             // full kitty encodes every key as CSI-u
        };

        if !should_encode {
            // Plain / shift printable, a ⌘ shortcut in legacy mode, or an
            // ⌥-composing key: let it propagate to the platform (NSTextInputClient
            // → IME compose or `insertText` commit) or to app keybindings. The pty
            // is not touched here.
            return;
        }

        let keycode = self.keycode_probe.as_ref().and_then(|probe| probe());
        let Some(mut input) = build_key_input(keystroke, event, keycode, false) else {
            return;
        };
        // For a Meta chord the inserted text is the OS-composed glyph (e.g. ⌥a →
        // "å"); Meta must send `ESC` + the *base* key, so drop that text and let
        // the encoder use the base char.
        if alt_meta {
            input.text = None;
        }
        if let Some(bytes) = encoder_config(mode, false).encode(&input) {
            self.write_pty(&bytes, cx);
        }
        // Terminal-owned: consume it even when the encoder produced nothing (a
        // legacy modified-key *repeat*, which encodes only on the initial press).
        // Consuming still is deliberate — letting such a key propagate would reach
        // AppKit's unhandled-key path and beep. Chords that *should* yield bytes
        // do (e.g. Ctrl+Shift+C degrades to 0x03 in `legacy_char_sequence`).
        cx.stop_propagation();
        cx.notify();
    }

    /// gpui flagsChanged: a bare modifier key (Shift/Ctrl/Alt/⌘) went down or up.
    /// This is the kitty **modifiers-as-functional-keys** report — under
    /// REPORT_ALL_KEYS the app sees each bare modifier as `CSI 57441 u` (left
    /// shift) etc., press and (with event reporting) release. Every other mode
    /// ignores it. The specific left/right key comes from the flagsChanged keyCode
    /// side-channel; press vs release is computed from the new aggregate modifier
    /// state (see [`build_modifier_input`]). While composing, the encoder still
    /// reports bare modifiers (kitty's composition rule) — the composing flag is
    /// threaded through so it can.
    fn on_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mode = self.current_mode(cx);
        if !mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) {
            return; // bare-modifier reports are a report-all-keys feature only
        }
        let Some(keycode) = self.keycode_probe.as_ref().and_then(|probe| probe()) else {
            return; // no keyCode side-channel -> cannot resolve which modifier key
        };
        let composing = self.ime.is_composing();
        let Some(input) = build_modifier_input(keycode, &event.modifiers, composing) else {
            return; // not a bare modifier key (or unmapped keyCode)
        };
        if let Some(bytes) = encoder_config(mode, false).encode(&input) {
            self.write_pty(&bytes, cx);
        }
    }

    // -- mouse, selection, paste/copy, focus reporting (slice 3) ---------------

    /// Hit-test a window pixel position to a grid cell, using the bounds the
    /// element published on its last paint (`paint_bounds`). Returns viewport +
    /// buffer coordinates, or `None` before the first paint / spawn.
    fn hit_cell(&self, pos: Point<Pixels>, cx: &App) -> Option<Hit> {
        let bounds = self.paint_bounds.get()?;
        let term_arc = self.handle.read(cx).term()?;
        let (rows, cols, display_offset) = {
            let term = term_arc.lock();
            (
                term.screen_lines(),
                term.columns(),
                term.grid().display_offset(),
            )
        };
        let grid_top = grid_top_y(bounds, self.metrics, rows);
        let rel_x = f32::from(pos.x) - f32::from(bounds.origin.x);
        let rel_y = f32::from(pos.y) - grid_top;
        let (col, vrow) = mouse::cell_from_offset(
            rel_x,
            rel_y,
            self.metrics.cell_w,
            self.metrics.cell_h,
            cols,
            rows,
        );
        Some(Hit {
            col,
            vrow,
            buffer_line: vrow as i32 - display_offset as i32,
        })
    }

    /// Encode + write one VT mouse report for `action` on `button` at `hit`.
    fn send_mouse_report(
        &self,
        mode: TermMode,
        button: VtButton,
        action: MouseAction,
        hit: &Hit,
        m: gpui::Modifiers,
        cx: &App,
    ) {
        let input = MouseInput {
            button,
            action,
            col: hit.col,
            line: hit.vrow,
            modifiers: mouse::report_modifiers(m),
        };
        if let Some(bytes) = encode_mouse(mouse::protocol(mode), &input) {
            self.write_pty(&bytes, cx);
        }
    }

    /// gpui mouse-down: a VT press report (app reporting, no Shift override) or
    /// the start of a local selection drag.
    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mode = self.current_mode(cx);
        let m = event.modifiers;

        // App mouse reporting, unless Shift forces a local selection.
        if mouse::reporting_active(mode) && !m.shift {
            if let (Some(button), Some(hit)) =
                (mouse::vt_button(event.button), self.hit_cell(event.position, cx))
            {
                self.send_mouse_report(mode, button, MouseAction::Press, &hit, m, cx);
                self.last_report_cell = Some((hit.col, hit.vrow));
            }
            cx.stop_propagation();
            return;
        }

        // Local selection: only the left button starts one. A bare click collapses
        // any prior selection; a subsequent drag rebuilds it from this anchor.
        if event.button == MouseButton::Left {
            if let Some(hit) = self.hit_cell(event.position, cx) {
                self.drag_anchor = Some((hit.buffer_line, hit.col));
                self.handle.read(cx).clear_selection();
                cx.notify();
            }
        }
    }

    /// gpui mouse-move: extend an active local selection, or emit a motion report.
    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((anchor_line, anchor_col)) = self.drag_anchor {
            // The button was released (possibly outside the pane, so no mouse-up
            // reached us) — stop extending the selection.
            if event.pressed_button != Some(MouseButton::Left) {
                self.drag_anchor = None;
                return;
            }
            if let Some(hit) = self.hit_cell(event.position, cx) {
                self.handle
                    .read(cx)
                    .set_selection((anchor_line, anchor_col), (hit.buffer_line, hit.col));
                cx.notify();
            }
            return;
        }

        let mode = self.current_mode(cx);
        if !mouse::reporting_active(mode) || event.modifiers.shift {
            return;
        }
        if !mouse::reports_motion(mode, event.pressed_button.is_some()) {
            return;
        }
        if let Some(hit) = self.hit_cell(event.position, cx) {
            // One report per cell crossed, not per pixel of travel.
            if self.last_report_cell == Some((hit.col, hit.vrow)) {
                return;
            }
            self.last_report_cell = Some((hit.col, hit.vrow));
            let button = event
                .pressed_button
                .and_then(mouse::vt_button)
                .unwrap_or(VtButton::None);
            self.send_mouse_report(mode, button, MouseAction::Motion, &hit, event.modifiers, cx);
        }
    }

    /// gpui mouse-up: end a local selection drag (keeping the selection) or emit a
    /// release report.
    fn on_mouse_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.drag_anchor.is_some() && event.button == MouseButton::Left {
            // Selection persists (for ⌘C); nothing is sent to the pty.
            self.drag_anchor = None;
            return;
        }
        let mode = self.current_mode(cx);
        if !mouse::reporting_active(mode) || event.modifiers.shift {
            return;
        }
        if let (Some(button), Some(hit)) =
            (mouse::vt_button(event.button), self.hit_cell(event.position, cx))
        {
            self.send_mouse_report(mode, button, MouseAction::Release, &hit, event.modifiers, cx);
        }
        // Consume the up while the app is reporting, matching the press.
        cx.stop_propagation();
    }

    /// A left button-up that landed outside the pane still ends a drag cleanly
    /// (the in-bounds `on_mouse_up` never fired for it).
    fn on_mouse_up_out(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.drag_anchor = None;
    }

    /// ⌘V: paste the clipboard, bracketed-wrapped when the app enabled DECSET
    /// 2004 (`bracketed_paste_active`), else passed through raw. R7's drag-drop
    /// reuses this same wrap seam.
    fn paste_clipboard(&self, cx: &App) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let active = self.handle.read(cx).session().bracketed_paste_active();
        let bytes = wrap_bracketed_paste(text.as_bytes(), active);
        self.write_pty(&bytes, cx);
    }

    /// ⌘C: copy a live selection to the pasteboard. Returns `true` iff there was
    /// a non-empty selection to copy (the caller then consumes the key).
    fn copy_selection(&self, cx: &App) -> bool {
        match self.handle.read(cx).selection_text() {
            Some(text) if !text.is_empty() => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                true
            }
            _ => false,
        }
    }

    /// Emit a DECSET-1004 focus report when the combined focus predicate changes.
    /// Called from `render` with the same value the caret uses. Edge-triggered:
    /// the first call seeds the state without emitting (so startup never sends a
    /// spurious `ESC[I`); later transitions send `ESC[I` (gained) / `ESC[O`
    /// (lost) when the app has focus reporting enabled.
    fn report_focus_change(&mut self, focused: bool, cx: &App) {
        if self.last_focus_reported == Some(focused) {
            return;
        }
        let seed = self.last_focus_reported.is_none();
        self.last_focus_reported = Some(focused);
        if seed {
            return;
        }
        if self.current_mode(cx).contains(TermMode::FOCUS_IN_OUT) {
            let seq: &[u8] = if focused { b"\x1b[I" } else { b"\x1b[O" };
            self.write_pty(seq, cx);
        }
    }

    // -- IME adapter (called by `TermInputHandler`) ----------------------------

    /// `selectedRange` — always a valid (possibly collapsed) range.
    pub(crate) fn ime_selected_range_utf16(&self) -> Range<usize> {
        self.ime.selected_range_utf16()
    }

    /// `markedRange` — `Some` iff composing (what routes keys to the IME first).
    pub(crate) fn ime_marked_range_utf16(&self) -> Option<Range<usize>> {
        self.ime.marked_range_utf16()
    }

    /// `attributedSubstringForProposedRange` — clamped preedit substring + range.
    pub(crate) fn ime_text_for_range(&self, range: Range<usize>) -> Option<(String, Range<usize>)> {
        self.ime.text_for_range_utf16(range)
    }

    /// `setMarkedText:` — update the preedit (no pty write) and repaint.
    pub(crate) fn ime_set_marked(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        sel: Option<Range<usize>>,
        cx: &mut Context<Self>,
    ) {
        self.ime.set_marked_text(range, text, sel);
        cx.notify();
    }

    /// `insertText:` — commit. Committed IME text is **data**: write it straight
    /// to the pty (never through the key encoder). If it ended a composition,
    /// schedule the end-of-cycle disarm so a later bare Enter still sends CR.
    pub(crate) fn ime_commit(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        let outcome = self.ime.commit_text(range, text);
        self.write_pty(outcome.pty_text.as_bytes(), cx);
        if outcome.was_composing {
            // End-of-native-key-cycle disarm: runs after any synchronous
            // `doCommandBySelector` re-dispatch, before the next keypress, so a
            // commit with no same-cycle Enter re-dispatch (e.g. Pinyin
            // Space-commit) cannot swallow a LATER bare Enter.
            cx.spawn(async move |this, cx| {
                this.update(cx, |view, _| view.ime.disarm_commit_swallow())
                    .ok();
            })
            .detach();
        }
        cx.notify();
    }

    /// `unmarkText` — accept the pending composition as typed (focus loss /
    /// input-source switch). Does not arm the Enter swallow.
    pub(crate) fn ime_unmark(&mut self, cx: &mut Context<Self>) {
        if let Some(pending) = self.ime.unmark() {
            self.write_pty(pending.as_bytes(), cx);
        }
        cx.notify();
    }

    /// `firstRectForCharacterRange` — the candidate-window anchor. **Never `None`**
    /// (the zed#46055 fix): always a rect at the grid cursor cell, in window px.
    /// For a sub-range query while composing it advances into the rendered preedit
    /// overlay (Terminal.app parity), so a multi-clause candidate list tracks the
    /// caret. `element_bounds` is the grid element's bounds this frame.
    pub(crate) fn ime_anchor_bounds(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Bounds<Pixels> {
        let m = self.metrics;
        // The grid cursor cell in viewport coordinates (row honours the scroll/
        // display offset, clamped on-screen). A full-screen TUI that parks or
        // hides the hardware cursor still has a grid cursor point, so this is
        // total by construction — there is no path that yields "no cursor".
        let cursor = self.handle.read(cx).term().map(|term_arc| {
            let term = term_arc.lock();
            let content = term.renderable_content();
            let display_offset = content.display_offset as i32;
            let screen_rows = term.screen_lines();
            let cols = term.columns();
            let cp = content.cursor.point;
            let vr = (cp.line.0 + display_offset).clamp(0, screen_rows.saturating_sub(1) as i32);
            (vr as usize, cp.column.0.min(cols.saturating_sub(1)), screen_rows)
        });
        let (row, col, rows) = cursor.unwrap_or((0, 0, 0));
        let grid_top = grid_top_y(element_bounds, m, rows);
        let mut x = f32::from(element_bounds.origin.x) + col as f32 * m.cell_w;
        let y = grid_top + row as f32 * m.cell_h;

        // Sub-range queries anchor within the rendered preedit overlay; range
        // start 0 (or idle) is exactly the cursor cell.
        if self.ime.is_composing() && range_utf16.start > 0 {
            let preedit = self.ime.preedit().to_string();
            let byte = utf16_to_byte(&preedit, range_utf16.start);
            let run = TextRun {
                len: preedit.len(),
                font: term_font(self.font_family.clone()),
                color: gpui::black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let shaped = window.text_system().shape_line(
                SharedString::from(preedit),
                px(self.font_px),
                &[run],
                None,
            );
            x += f32::from(shaped.x_for_index(byte));
        }

        Bounds {
            origin: point(px(x), px(y)),
            size: size(px(m.cell_w), px(m.cell_h)),
        }
    }
}

/// A plain monospace [`Font`] for the given family (preedit shaping / anchor
/// measurement). Attributes are irrelevant to the metrics the anchor needs.
fn term_font(family: SharedString) -> Font {
    Font {
        family,
        features: FontFeatures::default(),
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
        fallbacks: None,
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Take focus once (idempotent — `Window::focus` early-returns if this
        // handle already holds it) so the caret's computed focus state is live
        // without a stored flag. R13 will own focus routing across panes.
        window.focus(&self.focus_handle, cx);

        let caret_solid = self.focus_handle.is_focused(window) && window.is_window_active();

        // DECSET-1004 focus in/out rides the same predicate as the caret: emit a
        // report on the change edge (window activation calls `refresh()`, so this
        // render re-runs when activation flips, not just on focus-handle changes).
        self.report_focus_change(caret_solid, cx);

        // Snapshot the preedit for this frame's inline overlay (byte range for the
        // shaped runs). The IME wiring (input-handler registration + preedit
        // paint) is threaded into the element so it shares the grid geometry.
        let preedit = if self.ime.is_composing() {
            let text = self.ime.preedit().to_string();
            let sel16 = self.ime.selected_range_utf16();
            let sel_bytes =
                utf16_to_byte(&text, sel16.start)..utf16_to_byte(&text, sel16.end);
            Some((SharedString::from(text), sel_bytes))
        } else {
            None
        };
        let ime = ImeInput {
            focus_handle: self.focus_handle.clone(),
            view: cx.entity(),
            preedit,
        };

        let element = TerminalElement::new(
            self.handle.read(cx),
            &self.theme,
            self.accent,
            self.font_family.clone(),
            self.font_px,
            self.metrics,
            caret_solid,
            ime,
            self.paint_bounds.clone(),
        );

        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .on_any_mouse_down(cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up_out))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_key_up(cx.listener(Self::on_key_up))
            .on_modifiers_changed(cx.listener(Self::on_modifiers_changed))
            .child(element)
    }
}

impl TerminalView {
    /// Wheel / trackpad → line-stepped scrollback scroll, or VT wheel reports when
    /// the app requests mouse reporting (and Shift, the local override, is not
    /// held). gpui's convention is that a **positive** `delta.y` reveals earlier
    /// content, which for a terminal means scrolling **into history** — so the
    /// fractional line count derived from the delta is passed straight through to
    /// [`TerminalSessionHandle::scroll_lines`] (positive = toward history). The
    /// handle keeps the sub-line remainder as the deferred smooth-scroll seam;
    /// GPUI main pixel-snaps, so what actually paints is line-stepped.
    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // `pixel_delta` resolves both the precise (pixels) and coarse (lines)
        // wheel variants against the cell height; dividing back out yields a
        // fractional line count either way.
        let cell_h = self.metrics.cell_h;
        let dy: f32 = event.delta.pixel_delta(px(cell_h)).y.into();
        let lines = dy / cell_h;

        // Under app mouse reporting (and without the local-override Shift), the
        // wheel is a VT event, not local scrollback: emit button-64/65 reports at
        // the pointer cell. Positive `lines` reveals earlier content, i.e. wheel
        // **up** (button 64). Whole cells are reported; the remainder is kept so a
        // slow trackpad still eventually reports (like the scrollback accumulator).
        let mode = self.current_mode(cx);
        if mouse::reporting_active(mode) && !event.modifiers.shift {
            self.wheel_accum += lines;
            let steps = self.wheel_accum.trunc();
            self.wheel_accum -= steps;
            let count = (steps.abs() as i32).min(WHEEL_REPORT_MAX);
            if count > 0 {
                if let Some(hit) = self.hit_cell(event.position, cx) {
                    let button = if steps > 0.0 {
                        VtButton::WheelUp
                    } else {
                        VtButton::WheelDown
                    };
                    for _ in 0..count {
                        self.send_mouse_report(
                            mode,
                            button,
                            MouseAction::Press,
                            &hit,
                            event.modifiers,
                            cx,
                        );
                    }
                }
            }
            cx.stop_propagation();
            return;
        }

        if lines != 0.0 {
            self.handle.update(cx, |handle, hcx| {
                handle.scroll_lines(lines);
                hcx.notify();
            });
        }
    }
}
