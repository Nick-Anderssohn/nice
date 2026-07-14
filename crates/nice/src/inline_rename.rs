//! The shared inline-rename field — a small cursor-capable text editor that all
//! three inline renames mount: the file-browser row, the sidebar tab title, and
//! the toolbar pane pill.
//!
//! ## One editor, three call sites
//!
//! Every field is backed by the pure [`nice_model::file_browser::TextFieldEditor`]
//! (`{text, cursor, selection}` over char offsets): printable input inserts at the
//! caret, Backspace/Delete edit at the caret, Left/Right move it, Shift+Arrow
//! extends the selection, ⌘A selects all, and a click repositions the caret. The
//! *commit* semantics differ per caller ([`nice_model::TabModel::rename_tab`] vs
//! [`nice_model::TabModel::rename_pane`] vs the file-browser's validate+modal
//! path), so the key handler and the click handler are injected — this module
//! owns only the chrome, the caret/selection rendering, the pure key→editor
//! dispatch ([`dispatch_rename_key`]), and the click-x→char-index hit-test
//! ([`char_index_for_click`]).
//!
//! ## Escape is the owner's, not the field's
//!
//! [`dispatch_rename_key`] intentionally leaves Escape [`RenameKeyOutcome::Ignored`]:
//! each owner cancels the rename through its own Esc binding (the sidebar shell's
//! Esc action; the pill's own key listener), the replacement for the DO-NOT-PORT
//! `NSEvent` cancel monitor. That must still fire, so the field never consumes it.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    canvas, div, px, App, FocusHandle, Hsla, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, ParentElement, Rgba, SharedString, Styled, TextRun, Window,
};

use nice_model::file_browser::{char_index_for_x, TextFieldEditor, TextFieldKey};
use nice_theme::chrome_geometry::INNER_CORNER_RADIUS;

/// What feeding one keystroke to an in-flight rename field should do, after the
/// editing keys have already been applied to the editor.
pub(crate) enum RenameKeyOutcome {
    /// Return/Enter — the caller commits the draft (its own `rename_*` model call).
    Commit,
    /// The editor was mutated (insert / delete / caret move / selection change):
    /// the caller should `cx.notify()` and consume the key.
    Edited,
    /// A key this field does not handle (Escape, a ⌘/⌃ chord that isn't ⌘A, …):
    /// the caller leaves it to propagate. **Escape is intentionally Ignored** so
    /// the owner's Esc binding cancels the rename.
    Ignored,
}

/// Apply one keystroke to the in-flight rename `editor`, returning what the caller
/// should do. Pure over the editor model (no gpui state) so the exact editing
/// rule is unit-tested in [`nice_model::file_browser::text_field`]:
///
/// * Return/Enter → [`RenameKeyOutcome::Commit`] (editor untouched).
/// * Backspace / Delete → delete at the caret (or the selection).
/// * Left / Right (with Shift → extend) → move / extend the caret.
/// * ⌘A → select all.
/// * a bare printable char (no ⌘/⌃, a non-control `key_char`) → insert at the caret.
/// * anything else (Escape, ⌘-chords) → [`RenameKeyOutcome::Ignored`].
///
/// `capslock` is the live Caps-Lock state (read from `window.capslock().on` — it
/// is NOT carried in `keystroke.modifiers`). GPUI's macOS backend builds
/// `key_char` without the alphaLock bit, so an ASCII letter arrives
/// un-capslocked; when Caps Lock is on we flip that letter's case here, on top of
/// any Shift the `key_char` already reflects (so Shift+Caps nets lowercase).
/// Digits, punctuation, and non-ASCII characters are unaffected.
pub(crate) fn dispatch_rename_key(
    editor: &mut TextFieldEditor,
    key: &str,
    key_char: Option<&str>,
    shift: bool,
    platform_mod: bool,
    control_mod: bool,
    capslock: bool,
) -> RenameKeyOutcome {
    match key {
        "enter" | "return" => RenameKeyOutcome::Commit,
        "backspace" => {
            editor.apply_key(TextFieldKey::Backspace);
            RenameKeyOutcome::Edited
        }
        "delete" => {
            editor.apply_key(TextFieldKey::ForwardDelete);
            RenameKeyOutcome::Edited
        }
        "left" => {
            editor.apply_key(if shift {
                TextFieldKey::ShiftLeft
            } else {
                TextFieldKey::Left
            });
            RenameKeyOutcome::Edited
        }
        "right" => {
            editor.apply_key(if shift {
                TextFieldKey::ShiftRight
            } else {
                TextFieldKey::Right
            });
            RenameKeyOutcome::Edited
        }
        "a" if platform_mod => {
            editor.apply_key(TextFieldKey::SelectAll);
            RenameKeyOutcome::Edited
        }
        _ => {
            if !platform_mod && !control_mod {
                if let Some(ch) = key_char.and_then(|s| s.chars().next()) {
                    if !ch.is_control() {
                        // Caps Lock toggles letter case on top of Shift (which
                        // key_char already applied): no-shift lowercase → upper,
                        // shift upper → lower. Scoped to ASCII letters — Caps Lock
                        // doesn't touch digits/punct, and Unicode case-mapping is
                        // out of scope (see the plan's Risks).
                        let ch = if capslock && ch.is_ascii_alphabetic() {
                            if ch.is_ascii_lowercase() {
                                ch.to_ascii_uppercase()
                            } else {
                                ch.to_ascii_lowercase()
                            }
                        } else {
                            ch
                        };
                        editor.apply_key(TextFieldKey::Char(ch));
                        return RenameKeyOutcome::Edited;
                    }
                }
            }
            RenameKeyOutcome::Ignored
        }
    }
}

/// Apply a click at char boundary `index` to the rename `editor`, honoring the
/// platform multi-click convention the three call sites share: a single click
/// drops the caret, a double-click selects the word under the pointer, and a
/// triple-click (or more) selects the whole field (the single-line "line").
/// Centralizes the policy so each call site's click handler stays a one-liner and
/// the 1/2/3-click mapping is unit-tested in one place.
pub(crate) fn apply_rename_click(editor: &mut TextFieldEditor, index: usize, click_count: usize) {
    match click_count {
        2 => editor.select_word_at(index),
        n if n >= 3 => editor.select_all(),
        _ => editor.place_cursor(index),
    }
}

/// The active field's text split at its selection so the caller renders a caret
/// (collapsed) or a highlighted range plus pre/post text.
#[derive(Clone)]
pub(crate) struct EditSpans {
    pub(crate) pre: String,
    pub(crate) sel: String,
    pub(crate) post: String,
    pub(crate) collapsed: bool,
}

impl EditSpans {
    /// The full field text (pre + sel + post) — what the click hit-test shapes.
    fn full_text(&self) -> String {
        format!("{}{}{}", self.pre, self.sel, self.post)
    }
}

/// Split an editor's text at its selection for rendering.
pub(crate) fn edit_spans(editor: &TextFieldEditor) -> EditSpans {
    let text: Vec<char> = editor.text().chars().collect();
    let (s, e) = editor.selection();
    EditSpans {
        pre: text[..s].iter().collect(),
        sel: text[s..e].iter().collect(),
        post: text[e..].iter().collect(),
        collapsed: s == e,
    }
}

/// The chrome + text colours for the field. `bg`/`border` are the shared field
/// look (background3 fill + line_strong border, the restyle that shipped);
/// `text` is the glyph colour; `caret` a full-alpha bar for the collapsed cursor;
/// `selection` the highlight fill behind a selected range.
#[derive(Clone, Copy)]
pub(crate) struct FieldColors {
    pub(crate) bg: Rgba,
    pub(crate) border: Rgba,
    pub(crate) text: Rgba,
    pub(crate) caret: Rgba,
    pub(crate) selection: Rgba,
}

/// The field's painted geometry, written by two layout probes each paint:
///
/// * `text_left` — the TEXT RUN's left edge in window coordinates. The probe
///   sits inside the padding-less, border-less text row, so its box left IS the
///   x that glyph offsets are measured from. This is what the click hit-test
///   subtracts. (Probing the outer FIELD box here was the click off-by-one bug:
///   taffy positions an `absolute().inset_0()` child relative to its direct
///   parent's box inside the border, so a probe on the field recorded
///   `text_left − 6px` — the field's horizontal padding — and every click read
///   ~6px right, one narrow glyph.)
/// * `field_left` — the outer field box's probe, kept so scenarios can
///   cross-check the two probes against each other: `text_left − field_left`
///   must equal the field's horizontal padding (6px), which catches a
///   regression to the field-box bias without trusting either probe alone.
#[derive(Clone, Copy, Default)]
pub(crate) struct FieldProbe {
    pub(crate) field_left: f32,
    pub(crate) text_left: f32,
}

/// Map a click at window-x `click_x` to a char-boundary index into `text`, using
/// the window's text system to measure each glyph advance at `text_size`.
/// `text_left` is the text's left edge in window coordinates (captured by the
/// field's layout probe). Rounds to the nearest boundary via the pure
/// [`char_index_for_x`]; a click past the trailing edge lands the caret at the
/// end.
pub(crate) fn char_index_for_click(
    window: &Window,
    text: &str,
    text_size: f32,
    text_left: f32,
    click_x: f32,
) -> usize {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return 0;
    }
    // Shape the field text with the window's base font at the field's size (color
    // is irrelevant to advances). One run: the field is single-font, single-size.
    let run = TextRun {
        len: text.len(),
        font: window.text_style().font(),
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line(SharedString::from(text.to_string()), px(text_size), &[run], None);
    // Boundary x for each char index 0..=n (byte offset → x). `x_for_index` takes
    // a UTF-8 byte offset, so accumulate byte lengths for multi-byte names.
    let mut boundaries: Vec<f32> = Vec::with_capacity(chars.len() + 1);
    boundaries.push(0.0);
    let mut byte = 0usize;
    for ch in &chars {
        byte += ch.len_utf8();
        boundaries.push(f32::from(line.x_for_index(byte)));
    }
    char_index_for_x(&boundaries, click_x - text_left)
}

/// The x-offset (from the text's left edge, in pixels) of char-boundary `index`
/// in `text` at `text_size` — the inverse of [`char_index_for_click`]. Used by
/// self-test scenarios to synthesize a click at a known boundary and assert the
/// hit-test round-trips.
pub(crate) fn char_boundary_x(window: &Window, text: &str, text_size: f32, index: usize) -> f32 {
    let run = TextRun {
        len: text.len(),
        font: window.text_style().font(),
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line(SharedString::from(text.to_string()), px(text_size), &[run], None);
    let byte: usize = text.chars().take(index).map(char::len_utf8).sum();
    f32::from(line.x_for_index(byte))
}

/// The shared inline-rename field element: a focused, bordered box rendering the
/// editor's `spans` (pre text, then a caret or a highlighted selection, then post
/// text), wired to `on_key` and to a click handler that repositions the caret.
///
/// * `probe` is a per-field cell two layout probes write each paint (see
///   [`FieldProbe`]); the click handler reads `text_left` from it to turn a
///   window-x into a text-relative offset.
/// * `on_key` is the caller's key handler (built with `cx.listener` / a weak
///   entity); it dispatches through [`dispatch_rename_key`] and commits/cancels.
/// * `on_click_index` receives the hit-tested char index and the click count; the
///   caller applies it via [`apply_rename_click`] (single click places the caret,
///   double selects the word, triple selects all) and re-grabs field focus.
///
/// The click handler `stop_propagation`s so the press never reaches the row / tab
/// / pill mouse handler beneath it — the fix for "a click inside the field
/// restarts the edit". The pointer shows the text I-beam over the whole field.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rename_field(
    focus: &FocusHandle,
    spans: &EditSpans,
    key_context: &'static str,
    colors: FieldColors,
    text_size: f32,
    probe: Rc<Cell<FieldProbe>>,
    on_key: impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static,
    on_click_index: impl Fn(usize, usize, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    // Text-run probe: an absolute inset-0 canvas inside the padding-less,
    // border-less text row — taffy resolves it against the row's own box, whose
    // left edge IS the first glyph's x origin (see [`FieldProbe::text_left`]).
    let text_capture = probe.clone();
    let text_probe = canvas(
        |_, _, _| (),
        move |bounds, _, _, _| {
            text_capture.set(FieldProbe {
                text_left: f32::from(bounds.origin.x),
                ..text_capture.get()
            })
        },
    )
    .absolute()
    .inset_0();

    let mut text_row = div()
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .child(text_probe)
        .child(
            div()
                .text_size(px(text_size))
                .text_color(colors.text)
                .child(SharedString::from(spans.pre.clone())),
        );
    if spans.collapsed {
        // Caret: a thin full-alpha accent bar at the cursor (a selection tint is
        // invisible at 1px).
        text_row = text_row.child(div().w(px(1.0)).h(px(text_size + 1.0)).bg(colors.caret));
    } else {
        text_row = text_row.child(
            div()
                .bg(colors.selection)
                .text_size(px(text_size))
                .text_color(colors.text)
                .child(SharedString::from(spans.sel.clone())),
        );
    }
    text_row = text_row.child(
        div()
            .text_size(px(text_size))
            .text_color(colors.text)
            .child(SharedString::from(spans.post.clone())),
    );

    // Field-box probe: the scenario's independent cross-check anchor (see
    // [`FieldProbe::field_left`]). NOT used by the click hit-test.
    let field_capture = probe.clone();
    let field_probe = canvas(
        |_, _, _| (),
        move |bounds, _, _, _| {
            field_capture.set(FieldProbe {
                field_left: f32::from(bounds.origin.x),
                ..field_capture.get()
            })
        },
    )
    .absolute()
    .inset_0();

    let full_text = spans.full_text();
    div()
        .track_focus(focus)
        .key_context(key_context)
        .relative()
        .flex_1()
        .px(px(6.0))
        .py(px(2.0))
        .rounded(px(INNER_CORNER_RADIUS))
        .bg(colors.bg)
        .border(px(1.0))
        .border_color(colors.border)
        .cursor_text()
        .child(text_row)
        .child(field_probe)
        .on_key_down(on_key)
        .on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, window, app| {
            let idx = char_index_for_click(
                window,
                &full_text,
                text_size,
                probe.get().text_left,
                f32::from(e.position.x),
            );
            on_click_index(idx, e.click_count, window, app);
            // Swallow the press so the row / tab / pill handler beneath never sees
            // it — otherwise the click would re-trip the begin-rename gate.
            app.stop_propagation();
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(text: &str) -> TextFieldEditor {
        TextFieldEditor::new(text)
    }

    #[test]
    fn enter_requests_a_commit_without_touching_the_editor() {
        let mut e = ed("hello");
        assert!(matches!(
            dispatch_rename_key(&mut e, "enter", None, false, false, false, false),
            RenameKeyOutcome::Commit
        ));
        assert_eq!(e.text(), "hello");
    }

    #[test]
    fn a_bare_printable_char_inserts_at_the_caret() {
        let mut e = ed("ac");
        dispatch_rename_key(&mut e, "left", None, false, false, false, false); // caret between a,c
        assert!(matches!(
            dispatch_rename_key(&mut e, "b", Some("b"), false, false, false, false),
            RenameKeyOutcome::Edited
        ));
        assert_eq!(e.text(), "abc");
    }

    #[test]
    fn backspace_and_forward_delete_edit_at_the_caret() {
        let mut e = ed("abc");
        dispatch_rename_key(&mut e, "backspace", None, false, false, false, false);
        assert_eq!(e.text(), "ab");
        dispatch_rename_key(&mut e, "left", None, false, false, false, false);
        dispatch_rename_key(&mut e, "delete", None, false, false, false, false);
        assert_eq!(e.text(), "a");
    }

    #[test]
    fn left_right_move_and_shift_extends() {
        let mut e = ed("abc"); // caret at 3
        dispatch_rename_key(&mut e, "left", None, false, false, false, false);
        assert_eq!(e.cursor(), 2);
        dispatch_rename_key(&mut e, "left", None, true, false, false, false); // shift+left extends
        assert_eq!(e.selection(), (1, 2));
        dispatch_rename_key(&mut e, "right", None, false, false, false, false); // collapse to right edge
        assert_eq!(e.cursor(), 2);
        assert!(!e.has_selection());
    }

    #[test]
    fn command_a_selects_all_but_command_other_is_ignored() {
        let mut e = ed("name");
        assert!(matches!(
            dispatch_rename_key(&mut e, "a", Some("a"), false, true, false, false),
            RenameKeyOutcome::Edited
        ));
        assert_eq!(e.selection(), (0, 4));

        let mut e2 = ed("name");
        assert!(matches!(
            dispatch_rename_key(&mut e2, "c", Some("c"), false, true, false, false),
            RenameKeyOutcome::Ignored
        ));
        assert_eq!(e2.text(), "name");
    }

    #[test]
    fn escape_is_ignored_so_the_owner_binding_cancels() {
        let mut e = ed("name");
        assert!(matches!(
            dispatch_rename_key(&mut e, "escape", None, false, false, false, false),
            RenameKeyOutcome::Ignored
        ));
        assert_eq!(e.text(), "name");
    }

    #[test]
    fn a_control_chord_is_ignored() {
        let mut e = ed("name");
        assert!(matches!(
            dispatch_rename_key(&mut e, "a", Some("a"), false, false, true, false),
            RenameKeyOutcome::Ignored
        ));
        assert_eq!(e.text(), "name");
    }

    #[test]
    fn caps_lock_on_uppercases_a_typed_letter() {
        // key_char arrives lowercase (GPUI drops the alphaLock bit); Caps Lock on
        // must flip it to upper.
        let mut e = ed("");
        assert!(matches!(
            dispatch_rename_key(&mut e, "a", Some("a"), false, false, false, true),
            RenameKeyOutcome::Edited
        ));
        assert_eq!(e.text(), "A");
    }

    #[test]
    fn caps_lock_with_shift_lowercases_a_letter() {
        // Shift already made key_char "A"; Caps Lock on top cancels it back to "a".
        let mut e = ed("");
        assert!(matches!(
            dispatch_rename_key(&mut e, "a", Some("A"), true, false, false, true),
            RenameKeyOutcome::Edited
        ));
        assert_eq!(e.text(), "a");
    }

    #[test]
    fn caps_lock_leaves_digits_and_punctuation_unchanged() {
        let mut e = ed("");
        dispatch_rename_key(&mut e, "7", Some("7"), false, false, false, true);
        dispatch_rename_key(&mut e, "-", Some("-"), false, false, false, true);
        assert_eq!(e.text(), "7-");
    }

    #[test]
    fn apply_rename_click_maps_click_count_to_caret_word_and_all() {
        // Single click → caret (collapsed) at the boundary.
        let mut e = ed("foo bar");
        apply_rename_click(&mut e, 5, 1);
        assert_eq!(e.selection(), (5, 5));
        assert!(!e.has_selection());

        // Double click → the word under the pointer.
        let mut e = ed("foo bar");
        apply_rename_click(&mut e, 5, 2);
        assert_eq!(e.selection(), (4, 7)); // "bar"

        // Triple click (and beyond) → the whole field.
        let mut e = ed("foo bar");
        apply_rename_click(&mut e, 5, 3);
        assert_eq!(e.selection(), (0, 7));
        let mut e = ed("foo bar");
        apply_rename_click(&mut e, 5, 4);
        assert_eq!(e.selection(), (0, 7));
    }

    #[test]
    fn edit_spans_split_a_selection() {
        let mut e = TextFieldEditor::with_selection("foo.txt", 3);
        let s = edit_spans(&e);
        assert_eq!((s.pre.as_str(), s.sel.as_str(), s.post.as_str()), ("", "foo", ".txt"));
        assert!(!s.collapsed);
        assert_eq!(s.full_text(), "foo.txt");

        e.place_cursor(2);
        let c = edit_spans(&e);
        assert!(c.collapsed);
        assert_eq!((c.pre.as_str(), c.post.as_str()), ("fo", "o.txt"));
    }
}
