//! The shared inline-rename field — a small cursor-capable text editor that all
//! three inline renames mount: the file-browser row, the sidebar tab title, and
//! the toolbar pane pill.
//!
//! ## One editor, three call sites
//!
//! Every field is backed by the pure [`nice_model::file_browser::TextFieldEditor`]
//! (`{text, cursor, selection}` over char offsets): printable input inserts at the
//! caret, Backspace/Delete edit at the caret, Left/Right move it, Shift+Arrow
//! extends the selection, ⌥Arrow moves by word and ⌘Arrow to the text edges
//! (both composing with Shift), ⌥Delete deletes a word, ⌘A selects all,
//! ⌘C/⌘X/⌘V copy / cut / paste through the system clipboard, a click
//! repositions the caret, and a press-and-drag selects a range. The
//! *commit* semantics differ per caller ([`nice_model::TabModel::rename_tab`] vs
//! [`nice_model::TabModel::rename_pane`] vs the file-browser's validate+modal
//! path), so the key handler and the click handler are injected — this module
//! owns only the chrome, the caret/selection rendering, the pure key→editor
//! dispatch ([`dispatch_rename_key`]), and the click-x→char-index hit-test
//! ([`char_index_for_click`]).
//!
//! ## The clipboard is injected, not reached for
//!
//! [`dispatch_rename_key`] takes a [`RenameClipboard`] rather than an
//! [`App`]-shaped context, so the ⌘C/⌘X/⌘V rule stays exactly as unit-testable as
//! the rest of the dispatch (the tests pass an in-memory fake; production passes
//! the `App` the three call sites already hold). Pasted text is flattened to one
//! line — a tab title / pane name / filename cannot hold a newline or a tab.
//!
//! ## Escape is the owner's, not the field's
//!
//! [`dispatch_rename_key`] intentionally leaves Escape [`RenameKeyOutcome::Ignored`]:
//! each owner cancels the rename through its own Esc binding (the sidebar shell's
//! Esc action; the pill's own key listener), the replacement for the DO-NOT-PORT
//! `NSEvent` cancel monitor. That must still fire, so the field never consumes it.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    canvas, div, fill, point, px, relative, size, App, Bounds, ClipboardItem, DispatchPhase,
    Element, ElementId, FocusHandle, GlobalElementId, Hsla, InspectorElementId, InteractiveElement,
    IntoElement, KeyDownEvent, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PaintQuad, ParentElement, Pixels, Rgba, ShapedLine, SharedString, Style, Styled, TextAlign,
    TextRun, Window,
};

use nice_model::file_browser::{char_index_for_x, TextFieldEditor, TextFieldKey};
use nice_theme::chrome_geometry::INNER_CORNER_RADIUS;

/// What feeding one keystroke to an in-flight rename field should do, after the
/// editing keys have already been applied to the editor.
pub(crate) enum RenameKeyOutcome {
    /// Return/Enter — the caller commits the draft (its own `rename_*` model call).
    Commit,
    /// The editor was mutated (insert / delete / caret move / selection change)
    /// **or a clipboard chord was handled**: the caller should `cx.notify()` and
    /// consume the key. A ⌘C notifies nothing new (it only reads the editor), but
    /// reusing this outcome keeps the three call sites' match arms untouched and a
    /// spurious repaint is harmless.
    Edited,
    /// A key this field does not handle (Escape, a ⌘/⌃ chord that is none of ⌘A /
    /// ⌘C / ⌘X / ⌘V, …): the caller leaves it to propagate. **Escape is
    /// intentionally Ignored** so the owner's Esc binding cancels the rename.
    Ignored,
}

/// The clipboard seam the ⌘C/⌘X/⌘V arms of [`dispatch_rename_key`] go through.
/// Production is [`App`] (the three call sites already hold one); the dispatch
/// tests pass a tiny in-memory fake, which is what keeps the whole key→editor
/// rule unit-testable without a window or a real pasteboard.
pub(crate) trait RenameClipboard {
    /// The system clipboard's text, or `None` when it is empty or holds
    /// something that is not text.
    fn read_text(&mut self) -> Option<String>;
    /// Replace the system clipboard's contents with `text`.
    fn write_text(&mut self, text: String);
}

impl RenameClipboard for App {
    fn read_text(&mut self) -> Option<String> {
        self.read_from_clipboard().and_then(|item| item.text())
    }

    fn write_text(&mut self, text: String) {
        self.write_to_clipboard(ClipboardItem::new_string(text));
    }
}

/// Flatten pasted clipboard text to the ONE line this field can hold (a tab
/// title, a pane name, or a filename — none of which may contain a newline or a
/// tab).
///
/// Splits on control characters (`char::is_control`, which covers `\n`, `\r` and
/// `\t`), drops the empty segments adjacent controls produce, and joins what is
/// left with a single space: `"foo\r\n\tbar"` → `"foo bar"`. Ordinary spaces
/// inside a segment are preserved, so `"my file.txt"` pastes verbatim. An
/// all-control clipboard flattens to `""`, which the dispatch treats as "insert
/// nothing" (rather than as "delete the selection").
fn sanitize_pasted_text(text: &str) -> String {
    text.split(char::is_control)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Apply one keystroke to the in-flight rename `editor`, returning what the caller
/// should do. Pure over the editor model plus the injected `clipboard` (no gpui
/// state) so the exact editing rule is unit-tested here and in
/// [`nice_model::file_browser::text_field`]:
///
/// * Return/Enter → [`RenameKeyOutcome::Commit`] (editor untouched).
/// * Backspace / Delete → delete at the caret (or the selection);
///   with ⌥ → delete the previous / next word.
/// * Left / Right (with Shift → extend) → move / extend the caret;
///   with ⌥ → by a word, with ⌘ → to the start / end of the text.
/// * ⌘A → select all.
/// * ⌘C / ⌘X → write the selection to `clipboard` (⌘X also deletes it); with a
///   COLLAPSED selection both write nothing at all — the chord is still consumed.
/// * ⌘V → replace the selection (or insert at the caret) with the clipboard text,
///   flattened to one line by [`sanitize_pasted_text`]. Consumed even when the
///   clipboard is empty or holds no text.
/// * a bare printable char (no ⌘/⌃, a non-control `key_char`) → insert at the caret.
/// * anything else (Escape, the other ⌘/⌃ chords) → [`RenameKeyOutcome::Ignored`].
///
/// `alt` is the Option modifier. ⌘ wins over ⌥ on the arrows (a ⌘⌥ chord jumps
/// to the line edge), and Shift composes with either. ⌥ is deliberately NOT part
/// of the printable-insert gate below: Option+letter arrives as a composed
/// `key_char` (⌥a → `å`) and must keep inserting — only the arrow/delete chords
/// above are intercepted.
///
/// `capslock` is the live Caps-Lock state (read from `window.capslock().on` — it
/// is NOT carried in `keystroke.modifiers`). GPUI's macOS backend builds
/// `key_char` without the alphaLock bit, so an ASCII letter arrives
/// un-capslocked; when Caps Lock is on we flip that letter's case here, on top of
/// any Shift the `key_char` already reflects (so Shift+Caps nets lowercase).
/// Digits, punctuation, and non-ASCII characters are unaffected.
#[allow(clippy::too_many_arguments)]
pub(crate) fn dispatch_rename_key(
    editor: &mut TextFieldEditor,
    clipboard: &mut impl RenameClipboard,
    key: &str,
    key_char: Option<&str>,
    shift: bool,
    alt: bool,
    platform_mod: bool,
    control_mod: bool,
    capslock: bool,
) -> RenameKeyOutcome {
    match key {
        "enter" | "return" => RenameKeyOutcome::Commit,
        "backspace" => {
            editor.apply_key(if alt {
                TextFieldKey::DeleteWordBackward
            } else {
                TextFieldKey::Backspace
            });
            RenameKeyOutcome::Edited
        }
        "delete" => {
            editor.apply_key(if alt {
                TextFieldKey::DeleteWordForward
            } else {
                TextFieldKey::ForwardDelete
            });
            RenameKeyOutcome::Edited
        }
        "left" => {
            editor.apply_key(match (platform_mod, alt, shift) {
                (true, _, true) => TextFieldKey::ShiftLineStart,
                (true, _, false) => TextFieldKey::LineStart,
                (false, true, true) => TextFieldKey::ShiftWordLeft,
                (false, true, false) => TextFieldKey::WordLeft,
                (false, false, true) => TextFieldKey::ShiftLeft,
                (false, false, false) => TextFieldKey::Left,
            });
            RenameKeyOutcome::Edited
        }
        "right" => {
            editor.apply_key(match (platform_mod, alt, shift) {
                (true, _, true) => TextFieldKey::ShiftLineEnd,
                (true, _, false) => TextFieldKey::LineEnd,
                (false, true, true) => TextFieldKey::ShiftWordRight,
                (false, true, false) => TextFieldKey::WordRight,
                (false, false, true) => TextFieldKey::ShiftRight,
                (false, false, false) => TextFieldKey::Right,
            });
            RenameKeyOutcome::Edited
        }
        "a" if platform_mod => {
            editor.apply_key(TextFieldKey::SelectAll);
            RenameKeyOutcome::Edited
        }
        // The clipboard chords, guarded exactly like ⌘A above so a ⌃-chord (and
        // every other modifier combination) still falls through to Ignored.
        "c" | "x" | "v" if platform_mod => {
            if key == "v" {
                let pasted = clipboard
                    .read_text()
                    .map(|text| sanitize_pasted_text(&text))
                    .unwrap_or_default();
                // An empty / non-text / all-control clipboard inserts NOTHING —
                // it must not silently delete the selection the user is holding.
                // The chord is consumed either way (below), so it never leaks to
                // the app keymap mid-rename.
                if !pasted.is_empty() {
                    editor.insert_str(&pasted);
                }
            } else if let Some(selected) = editor.selected_text() {
                // ⌘C / ⌘X. A COLLAPSED selection writes nothing at all —
                // `NSTextField` leaves the clipboard alone rather than clobbering
                // whatever the user copied a moment ago with an empty string.
                clipboard.write_text(selected);
                if key == "x" {
                    editor.apply_key(TextFieldKey::Backspace);
                }
            }
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

/// The active field's text plus its selection, as the render pass needs them:
/// ONE string (shaped once at paint) and the selected range as char offsets.
///
/// This deliberately is not a pre/sel/post split anymore. The field used to
/// render three sibling text divs and measure clicks against a fourth,
/// separately-shaped copy of the string — two measurements that could (and did)
/// disagree. See [`rename_field`].
#[derive(Clone)]
pub(crate) struct FieldText {
    /// The full field text.
    pub(crate) text: String,
    /// The selection as `(start, end)` char offsets, `start <= end`. A collapsed
    /// range (`start == end`) is the caret.
    pub(crate) sel: (usize, usize),
}

/// The editor's text + selection, ready to render.
pub(crate) fn field_text(editor: &TextFieldEditor) -> FieldText {
    FieldText {
        text: editor.text(),
        sel: editor.selection(),
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

/// The field's painted geometry, written by the field's own paint pass. It is
/// the ONE measurement of the field's text: the caret bar, the selection
/// highlight, and the click hit-test all read it, so they cannot disagree.
///
/// * `text_left` — the TEXT RUN's left edge in window coordinates: the bounds
///   the text element was actually laid out at, so its left IS the x glyph
///   offsets are measured from. This is what the click hit-test subtracts.
///   (Probing the outer FIELD box here was the click off-by-one bug: taffy
///   positions an `absolute().inset_0()` child relative to its direct parent's
///   box inside the border, so a probe on the field recorded `text_left − 6px`
///   — the field's horizontal padding — and every click read ~6px right, one
///   narrow glyph.)
/// * `boundaries` — the x-offset (from `text_left`) of every char boundary
///   `0..=n`, measured by shaping the field text ONCE during paint with the text
///   style resolved at the element (`Window::text_style()` inside prepaint), the
///   very style the glyphs are then painted with.
/// * `field_left` / `field_top` / `field_height` — the outer field box's probe.
///   `field_left` is kept so scenarios can cross-check the two probes against
///   each other: `text_left − field_left` must equal the field's horizontal
///   padding (6px), which catches a regression to the field-box bias without
///   trusting either probe alone. `field_top` + `field_height` give the box's
///   vertical band, which is what a live scenario needs to aim a REAL
///   (CG-global) mouse press at the field: the boundary table supplies the x,
///   this supplies the y.
/// * `drag` — the live drag-select (Bug B), `Some(index)` between the press
///   that armed it and the release that ends it, holding the boundary the last
///   mouse-move extended to (so a move within the same glyph re-notifies
///   nobody). This is state, not geometry, but it belongs in the same cell: it
///   is the one thing that has to survive from the press (a mouse handler) to
///   the moves (window listeners re-registered by the NEXT paint), and the cell
///   is exactly the per-field paint↔mouse channel that outlives both.
///
/// Empty `boundaries` means "not painted yet" — every reader treats that as
/// boundary 0, since no click can reach a field that has never been drawn.
#[derive(Clone, Default)]
pub(crate) struct FieldProbe {
    pub(crate) field_left: f32,
    pub(crate) field_top: f32,
    pub(crate) field_height: f32,
    pub(crate) text_left: f32,
    pub(crate) boundaries: Vec<f32>,
    pub(crate) drag: Option<usize>,
}

/// The shared per-field paint→mouse channel: the field's paint writes it, the
/// field's mouse handler and the scenario seams read it.
pub(crate) type FieldProbeCell = Rc<RefCell<FieldProbe>>;

/// A fresh, unpainted probe cell (one per rename-capable view).
pub(crate) fn field_probe_cell() -> FieldProbeCell {
    Rc::new(RefCell::new(FieldProbe::default()))
}

/// Put the cell back to "not painted yet". Every rename-BEGIN must call this,
/// because the cell is per-VIEW, not per-edit: it outlives every field it has
/// ever measured, so without a reset a new edit starts on the PREVIOUS edit's
/// numbers.
///
/// The arm that makes this a correctness fix rather than tidiness is
/// [`FieldProbe::drag`]. Ending an edit while the button is still held (Escape
/// mid-drag) kills the window listeners with the field, so the release is never
/// observed and `drag` stays `Some(stale)`. The move handler's self-heal only
/// disarms on a move with the button NOT pressed; if the next edit in the same
/// view begins from a press the user is still holding, the new field's first
/// paint registers its move listener over that stale arm and the very next
/// held-button move extends the fresh editor — clobbering the basename
/// preselection with a drag nobody started. Resetting at begin removes the
/// window entirely. The stale `boundaries` are the same story one step milder:
/// a click before the new field's first paint would hit-test against the old
/// name's glyph table.
pub(crate) fn reset_field_probe(probe: &FieldProbeCell) {
    *probe.borrow_mut() = FieldProbe::default();
}

/// Map a click at window-x `click_x` to a char-boundary index, against the
/// boundary table the field's last paint recorded. Rounds to the nearest
/// boundary via the pure [`char_index_for_x`]; a click past the trailing edge
/// lands the caret at the end.
pub(crate) fn char_index_for_click(probe: &FieldProbe, click_x: f32) -> usize {
    char_index_for_x(&probe.boundaries, click_x - probe.text_left)
}

/// The x-offset (from the text's left edge, in pixels) of char-boundary `index`
/// as the field last PAINTED it — the inverse of [`char_index_for_click`], and
/// the same numbers the caret and selection are drawn from. `None` when the
/// field has not painted yet or `index` is past the end. Used by self-test
/// scenarios to synthesize a click at a known boundary.
pub(crate) fn char_boundary_x(probe: &FieldProbe, index: usize) -> Option<f32> {
    probe.boundaries.get(index).copied()
}

/// The field's text run: shapes the whole name ONCE per paint with the text
/// style resolved at this point in the element tree, paints those glyphs, and
/// paints the caret bar / selection highlight as overlays measured from that
/// same shaping — then publishes the boundary table into [`FieldProbe`] so the
/// click hit-test reads the very numbers that were drawn.
///
/// This replaced a pre/sel/post split across three sibling text divs whose
/// clicks were hit-tested against a FOURTH shaping done in the mouse handler
/// with the WINDOW BASE style. Measured on the real element (the
/// `visual_rename_caret` pixel gate, `WWWWiiiimmmm-1234567890.txt` at 14pt):
/// with an ancestor font family set — the toolbar pill and sidebar tab
/// configuration — the painted caret sat 2.9px off the clicked x at boundary 9
/// and 10.1px off at boundary 23 (avg advance 8.3px), i.e. the error grew with
/// the prefix, exactly the reported drift. Shaping once at paint makes both
/// numbers a sub-pixel rounding difference.
struct RenameTextElement {
    text: SharedString,
    /// Selection as char offsets (collapsed ⇒ caret).
    sel: (usize, usize),
    color: Rgba,
    caret: Rgba,
    selection: Rgba,
    probe: FieldProbeCell,
}

/// What [`RenameTextElement`]'s prepaint hands to its paint: the shaped line and
/// the caret/selection quads measured from it.
struct RenameTextPrepaint {
    line: Option<ShapedLine>,
    selection: Option<PaintQuad>,
    caret: Option<PaintQuad>,
}

impl IntoElement for RenameTextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for RenameTextElement {
    type RequestLayoutState = ();
    type PrepaintState = RenameTextPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        // The row is one line tall in the INHERITED line height — what the three
        // text divs used to resolve to, so the sidebar tab's line-height wrapper
        // (`sidebar_shell.rs`) still sizes the editing row exactly as before.
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        // THE measurement: the style resolved right here, mid-paint — family,
        // weight, features and size as the glyphs below will be drawn with, not
        // the window base style a mouse handler would have seen.
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let run = TextRun {
            len: self.text.len(),
            font: style.font(),
            color: Hsla::from(self.color),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window
            .text_system()
            .shape_line(self.text.clone(), font_size, &[run], None);

        // Boundary x for each char index 0..=n. `x_for_index` takes a UTF-8 byte
        // offset, so accumulate byte lengths for multi-byte names.
        let mut boundaries: Vec<f32> = Vec::with_capacity(self.text.chars().count() + 1);
        boundaries.push(0.0);
        let mut byte = 0usize;
        for ch in self.text.chars() {
            byte += ch.len_utf8();
            boundaries.push(f32::from(line.x_for_index(byte)));
        }
        {
            let mut probe = self.probe.borrow_mut();
            probe.text_left = f32::from(bounds.origin.x);
            probe.boundaries.clone_from(&boundaries);
        }
        let boundary_x = |i: usize| px(boundaries.get(i).copied().unwrap_or(0.0));

        let (start, end) = self.sel;
        let (selection, caret) = if start == end {
            // Caret: a thin full-alpha accent bar at the cursor (a selection tint
            // is invisible at 1px), centred on the line box like the glyphs.
            let h = font_size + px(1.0);
            let y = bounds.origin.y + (bounds.size.height - h) / 2.0;
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(bounds.origin.x + boundary_x(start), y),
                        size(px(1.0), h),
                    ),
                    self.caret,
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(bounds.origin.x + boundary_x(start), bounds.origin.y),
                        point(
                            bounds.origin.x + boundary_x(end),
                            bounds.origin.y + bounds.size.height,
                        ),
                    ),
                    self.selection,
                )),
                None,
            )
        };
        RenameTextPrepaint {
            line: Some(line),
            selection,
            caret,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        // Highlight first (behind the glyphs), then the text, then the caret bar
        // on top of it.
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection);
        }
        if let Some(line) = prepaint.line.take() {
            let _ = line.paint(
                bounds.origin,
                bounds.size.height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
        if let Some(caret) = prepaint.caret.take() {
            window.paint_quad(caret);
        }
    }
}

/// The shared inline-rename field element: a focused, bordered box rendering the
/// editor's `text` with a caret or a selection highlight, wired to `on_key` and
/// to a click handler that repositions the caret.
///
/// * `probe` is a per-field cell the field's paint writes (see [`FieldProbe`]);
///   the click handler reads `text_left` + `boundaries` back out of it, so the
///   hit-test measures the glyphs that were actually drawn.
/// * `on_key` is the caller's key handler (built with `cx.listener` / a weak
///   entity); it dispatches through [`dispatch_rename_key`] and commits/cancels.
/// * `on_click_index` receives the hit-tested char index and the click count; the
///   caller applies it via [`apply_rename_click`] (single click places the caret,
///   double selects the word, triple selects all) and re-grabs field focus.
/// * `on_click_index`'s drag-select sibling `on_drag_index` receives the char
///   index each mouse-move lands on while a drag-select is live; the caller
///   `extend_to`s the editor and `cx.notify()`s (see the drag-select section
///   below). It is a separate callback precisely so the click contract above
///   stays a single click policy: a drag never re-runs the 1/2/3-click mapping.
///
/// The click handler `stop_propagation`s so the press never reaches the row / tab
/// / pill mouse handler beneath it — the fix for "a click inside the field
/// restarts the edit", and what keeps a drag-select from arming the row's / tab's
/// / pill's own drag: a press those handlers never see arms nothing (the
/// file-browser row's `on_drag` file-drag source is additionally suppressed
/// outright while editing). The pointer shows the text I-beam over the whole
/// field.
///
/// ## Drag-select (press, move with the button held, release)
///
/// A single press arms the drag by recording the pressed boundary in
/// [`FieldProbe::drag`]; every subsequent mouse-move hit-tests against the SAME
/// paint-recorded boundary table the press used and hands the new index to
/// `on_drag_index`. The move/up listeners are registered at WINDOW level (not on
/// the field's own div, whose `on_mouse_move` fires only while the pointer is
/// inside its bounds) so the selection keeps extending when the pointer leaves
/// the small field horizontally — the gpui `input.rs` example's `is_selecting`
/// pattern, with the window-level registration it needs because this field is a
/// leaf element the size of one row.
///
/// Teardown is structural: window mouse listeners live for exactly one frame, so
/// they are re-registered by every paint of the field and simply cease to exist
/// the frame after the field stops being rendered (commit / cancel / blur). No
/// handler outlives the edit, which is why ending an edit mid-drag cannot mutate
/// a model that is no longer being shown.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rename_field(
    focus: &FocusHandle,
    text: &FieldText,
    key_context: &'static str,
    colors: FieldColors,
    text_size: f32,
    probe: FieldProbeCell,
    on_key: impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static,
    on_click_index: impl Fn(usize, usize, &mut Window, &mut App) + 'static,
    on_drag_index: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let text_row = div()
        .flex()
        .flex_row()
        .items_center()
        .flex_1()
        // The field pins its own size and colour; family / weight / line height
        // still cascade from the call site (the pill and the sidebar tab set the
        // terminal family on an ancestor), and the element below shapes with
        // whatever this resolves to.
        .text_size(px(text_size))
        .text_color(colors.text)
        .child(RenameTextElement {
            text: SharedString::from(text.text.clone()),
            sel: text.sel,
            color: colors.text,
            caret: colors.caret,
            selection: colors.selection,
            probe: probe.clone(),
        });

    // Two things that can only happen in the PAINT phase, on one zero-cost
    // overlay: the field-box probe (the scenario's independent cross-check
    // anchor — see [`FieldProbe::field_left`]; NOT used by the click hit-test),
    // and the window-level drag-select tracking. `Window::on_mouse_event` is
    // paint-only and its listeners are cleared after the next frame, so
    // registering here means they exist for exactly as long as the field is
    // drawn — that IS the teardown on commit/cancel/blur.
    let overlay_probe = probe.clone();
    let paint_overlay = canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            // 1. Scenario probe: the field-box geometry cross-check anchor.
            {
                let mut p = overlay_probe.borrow_mut();
                p.field_left = f32::from(bounds.origin.x);
                p.field_top = f32::from(bounds.origin.y);
                p.field_height = f32::from(bounds.size.height);
            }

            // 2. Drag-select tracking (PRODUCTION behavior — do not remove with
            // the probe above). Extend the selection while the button is held.
            // Capture phase, so a bubble-phase handler that consumes the move
            // cannot starve the drag; it never consumes the event itself (hover
            // keeps working).
            let move_probe = overlay_probe.clone();
            window.on_mouse_event(
                move |e: &MouseMoveEvent, phase, window: &mut Window, app: &mut App| {
                    if phase != DispatchPhase::Capture {
                        return;
                    }
                    let mut p = move_probe.borrow_mut();
                    let Some(last) = p.drag else {
                        return;
                    };
                    if e.pressed_button != Some(MouseButton::Left) {
                        // The press ended somewhere this field never saw (the
                        // edit closed mid-drag, say). Self-heal so a stale arm
                        // can never extend a LATER edit's selection.
                        p.drag = None;
                        return;
                    }
                    let idx = char_index_for_click(&p, f32::from(e.position.x));
                    if idx == last {
                        return;
                    }
                    p.drag = Some(idx);
                    drop(p);
                    on_drag_index(idx, window, app);
                },
            );

            // Any left release ends the drag, wherever it lands — capture phase
            // for the same reason: disarming must not depend on the release
            // surviving to the bubble phase.
            let up_probe = overlay_probe.clone();
            window.on_mouse_event(
                move |e: &MouseUpEvent, phase, _window: &mut Window, _app: &mut App| {
                    if phase == DispatchPhase::Capture && e.button == MouseButton::Left {
                        up_probe.borrow_mut().drag = None;
                    }
                },
            );
        },
    )
    .absolute()
    .inset_0();

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
        .child(paint_overlay)
        .on_key_down(on_key)
        .on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, window, app| {
            let idx = {
                let mut p = probe.borrow_mut();
                let idx = char_index_for_click(&p, f32::from(e.position.x));
                // Only a SINGLE press arms a drag-select. Extending from a
                // double/triple click would collapse the word / select-all the
                // moment the pointer twitched, and word-wise extension from a
                // double-click is deliberately out of scope.
                p.drag = (e.click_count == 1).then_some(idx);
                idx
            };
            on_click_index(idx, e.click_count, window, app);
            // Swallow the press so the row / tab / pill handler beneath never sees
            // it — otherwise the click would re-trip the begin-rename gate.
            app.stop_propagation();
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // In every `dispatch_rename_key` call below the trailing bools are, in
    // order: `shift, alt, platform_mod, control_mod, capslock`.

    fn ed(text: &str) -> TextFieldEditor {
        TextFieldEditor::new(text)
    }

    /// The in-memory stand-in for `gpui::App`'s pasteboard — the whole reason
    /// [`RenameClipboard`] exists, so the ⌘C/⌘X/⌘V rule is asserted with no
    /// window and no real system clipboard.
    #[derive(Default)]
    struct FakeClipboard {
        text: Option<String>,
    }

    impl RenameClipboard for FakeClipboard {
        fn read_text(&mut self) -> Option<String> {
            self.text.clone()
        }

        fn write_text(&mut self, text: String) {
            self.text = Some(text);
        }
    }

    /// A throwaway empty clipboard for the calls that are not about the
    /// clipboard at all (most of them). The clipboard tests bind their own.
    fn clip() -> FakeClipboard {
        FakeClipboard::default()
    }

    /// A clipboard pre-seeded with `text` — what the ⌘V tests paste from.
    fn seeded(text: &str) -> FakeClipboard {
        FakeClipboard {
            text: Some(text.to_string()),
        }
    }

    #[test]
    fn enter_requests_a_commit_without_touching_the_editor() {
        let mut e = ed("hello");
        assert!(matches!(
            dispatch_rename_key(&mut e, &mut clip(), "enter", None, false, false, false, false, false),
            RenameKeyOutcome::Commit
        ));
        assert_eq!(e.text(), "hello");
    }

    #[test]
    fn a_bare_printable_char_inserts_at_the_caret() {
        let mut e = ed("ac");
        dispatch_rename_key(&mut e, &mut clip(), "left", None, false, false, false, false, false); // caret between a,c
        assert!(matches!(
            dispatch_rename_key(&mut e, &mut clip(), "b", Some("b"), false, false, false, false, false),
            RenameKeyOutcome::Edited
        ));
        assert_eq!(e.text(), "abc");
    }

    #[test]
    fn backspace_and_forward_delete_edit_at_the_caret() {
        let mut e = ed("abc");
        dispatch_rename_key(&mut e, &mut clip(), "backspace", None, false, false, false, false, false);
        assert_eq!(e.text(), "ab");
        dispatch_rename_key(&mut e, &mut clip(), "left", None, false, false, false, false, false);
        dispatch_rename_key(&mut e, &mut clip(), "delete", None, false, false, false, false, false);
        assert_eq!(e.text(), "a");
    }

    #[test]
    fn left_right_move_and_shift_extends() {
        let mut e = ed("abc"); // caret at 3
        dispatch_rename_key(&mut e, &mut clip(), "left", None, false, false, false, false, false);
        assert_eq!(e.cursor(), 2);
        dispatch_rename_key(&mut e, &mut clip(), "left", None, true, false, false, false, false); // shift+left extends
        assert_eq!(e.selection(), (1, 2));
        dispatch_rename_key(&mut e, &mut clip(), "right", None, false, false, false, false, false); // collapse to right edge
        assert_eq!(e.cursor(), 2);
        assert!(!e.has_selection());
    }

    #[test]
    fn option_arrows_move_by_word_and_shift_extends() {
        let mut e = ed("foo.bar"); // caret at 7
        assert!(matches!(
            dispatch_rename_key(&mut e, &mut clip(), "left", None, false, true, false, false, false),
            RenameKeyOutcome::Edited
        ));
        assert_eq!(e.cursor(), 4);
        dispatch_rename_key(&mut e, &mut clip(), "left", None, false, true, false, false, false);
        assert_eq!(e.cursor(), 0);
        assert!(!e.has_selection());

        dispatch_rename_key(&mut e, &mut clip(), "right", None, false, true, false, false, false);
        assert_eq!(e.cursor(), 3);

        // ⌥⇧ extends instead of moving.
        dispatch_rename_key(&mut e, &mut clip(), "right", None, true, true, false, false, false);
        assert_eq!(e.selection(), (3, 7));
        dispatch_rename_key(&mut e, &mut clip(), "left", None, true, true, false, false, false);
        assert_eq!(e.selection(), (3, 4));
    }

    #[test]
    fn command_arrows_jump_to_the_text_edges_and_shift_extends() {
        let mut e = ed("foo.bar");
        dispatch_rename_key(&mut e, &mut clip(), "left", None, false, false, true, false, false);
        assert_eq!(e.cursor(), 0);
        assert!(!e.has_selection());
        dispatch_rename_key(&mut e, &mut clip(), "right", None, false, false, true, false, false);
        assert_eq!(e.cursor(), 7);

        let mut e2 = ed("foo.bar");
        dispatch_rename_key(&mut e2, &mut clip(), "left", None, true, false, true, false, false);
        assert_eq!(e2.selection(), (0, 7));

        let mut e3 = ed("foo.bar");
        dispatch_rename_key(&mut e3, &mut clip(), "left", None, false, false, true, false, false);
        dispatch_rename_key(&mut e3, &mut clip(), "right", None, true, false, true, false, false);
        assert_eq!(e3.selection(), (0, 7));
    }

    #[test]
    fn command_wins_over_option_on_the_arrows() {
        // A ⌘⌥← chord jumps to the line start, not one word left.
        let mut e = ed("foo.bar");
        dispatch_rename_key(&mut e, &mut clip(), "left", None, false, true, true, false, false);
        assert_eq!(e.cursor(), 0);
    }

    #[test]
    fn option_delete_keys_delete_by_word() {
        let mut e = ed("foo.bar"); // caret at 7
        assert!(matches!(
            dispatch_rename_key(&mut e, &mut clip(), "backspace", None, false, true, false, false, false),
            RenameKeyOutcome::Edited
        ));
        assert_eq!(e.text(), "foo.");

        let mut e2 = ed("foo.bar");
        dispatch_rename_key(&mut e2, &mut clip(), "left", None, false, false, true, false, false); // caret to 0
        dispatch_rename_key(&mut e2, &mut clip(), "delete", None, false, true, false, false, false);
        assert_eq!(e2.text(), ".bar");
    }

    #[test]
    fn option_composed_characters_still_insert() {
        // ⌥a arrives as key "a" with a composed key_char "å" — the alt chords
        // above must not swallow it (the printable fall-through gates on ⌘/⌃
        // only).
        let mut e = ed("");
        assert!(matches!(
            dispatch_rename_key(&mut e, &mut clip(), "a", Some("å"), false, true, false, false, false),
            RenameKeyOutcome::Edited
        ));
        assert_eq!(e.text(), "å");
        // ⌥⇧o → "Ø", likewise.
        dispatch_rename_key(&mut e, &mut clip(), "o", Some("Ø"), true, true, false, false, false);
        assert_eq!(e.text(), "åØ");
    }

    #[test]
    fn command_a_selects_all_but_command_other_is_ignored() {
        let mut e = ed("name");
        assert!(matches!(
            dispatch_rename_key(&mut e, &mut clip(), "a", Some("a"), false, false, true, false, false),
            RenameKeyOutcome::Edited
        ));
        assert_eq!(e.selection(), (0, 4));

        // A ⌘-chord the field does NOT claim still propagates (⌘K here — ⌘C/⌘X/⌘V
        // are the field's own, and have their own tests below).
        let mut e2 = ed("name");
        assert!(matches!(
            dispatch_rename_key(&mut e2, &mut clip(), "k", Some("k"), false, false, true, false, false),
            RenameKeyOutcome::Ignored
        ));
        assert_eq!(e2.text(), "name");
    }

    // MARK: - clipboard chords (⌘C / ⌘X / ⌘V)

    /// ⌘C on a selection writes exactly the selected text and does not touch the
    /// editor.
    #[test]
    fn command_c_copies_the_selection_without_editing() {
        let mut e = TextFieldEditor::with_selection("foo.txt", 3); // "foo" selected
        let mut cb = clip();
        assert!(matches!(
            dispatch_rename_key(&mut e, &mut cb, "c", Some("c"), false, false, true, false, false),
            RenameKeyOutcome::Edited
        ));
        assert_eq!(cb.text.as_deref(), Some("foo"));
        assert_eq!(e.text(), "foo.txt");
        assert_eq!(e.selection(), (0, 3));
    }

    /// ⌘C with a COLLAPSED selection leaves the clipboard exactly as it was —
    /// NSTextField never clobbers it with an empty string — while still
    /// consuming the chord.
    #[test]
    fn command_c_with_no_selection_leaves_the_clipboard_alone() {
        let mut e = ed("name"); // collapsed caret at the end
        let mut cb = seeded("previously copied");
        assert!(matches!(
            dispatch_rename_key(&mut e, &mut cb, "c", Some("c"), false, false, true, false, false),
            RenameKeyOutcome::Edited
        ));
        assert_eq!(cb.text.as_deref(), Some("previously copied"));
        assert_eq!(e.text(), "name");
    }

    /// ⌘X copies AND deletes, leaving a collapsed caret where the selection was.
    #[test]
    fn command_x_copies_the_selection_and_deletes_it() {
        let mut e = TextFieldEditor::with_selection("foo.txt", 3);
        let mut cb = clip();
        assert!(matches!(
            dispatch_rename_key(&mut e, &mut cb, "x", Some("x"), false, false, true, false, false),
            RenameKeyOutcome::Edited
        ));
        assert_eq!(cb.text.as_deref(), Some("foo"));
        assert_eq!(e.text(), ".txt");
        assert_eq!(e.selection(), (0, 0));
    }

    /// ⌘X with nothing selected deletes nothing (it is not a Backspace) and
    /// writes nothing.
    #[test]
    fn command_x_with_no_selection_is_inert() {
        let mut e = ed("name");
        let mut cb = seeded("previously copied");
        dispatch_rename_key(&mut e, &mut cb, "x", Some("x"), false, false, true, false, false);
        assert_eq!(cb.text.as_deref(), Some("previously copied"));
        assert_eq!(e.text(), "name");
    }

    /// ⌘V over a selection replaces it; at a collapsed caret it inserts.
    #[test]
    fn command_v_replaces_a_selection_and_inserts_at_a_caret() {
        let mut e = TextFieldEditor::with_selection("foo.txt", 3);
        let mut cb = seeded("bar");
        assert!(matches!(
            dispatch_rename_key(&mut e, &mut cb, "v", Some("v"), false, false, true, false, false),
            RenameKeyOutcome::Edited
        ));
        assert_eq!(e.text(), "bar.txt");
        assert_eq!(e.selection(), (3, 3));

        // Caret between "bar" and ".txt": a second paste inserts there.
        dispatch_rename_key(&mut e, &mut cb, "v", Some("v"), false, false, true, false, false);
        assert_eq!(e.text(), "barbar.txt");
        assert_eq!(e.cursor(), 6);
    }

    /// Multi-line / tabbed clipboard content flattens to the single line the
    /// field can hold, with ordinary spaces preserved.
    #[test]
    fn command_v_flattens_multiline_clipboard_content() {
        let mut e = ed("");
        let mut cb = seeded("foo\r\n\tbar baz");
        dispatch_rename_key(&mut e, &mut cb, "v", Some("v"), false, false, true, false, false);
        assert_eq!(e.text(), "foo bar baz");
    }

    /// An empty clipboard pastes NOTHING — in particular it must not delete the
    /// selection — but the chord is still consumed.
    #[test]
    fn command_v_with_an_empty_clipboard_leaves_the_editor_untouched() {
        let mut e = TextFieldEditor::with_selection("foo.txt", 3);
        let mut cb = clip(); // read_text() -> None
        assert!(matches!(
            dispatch_rename_key(&mut e, &mut cb, "v", Some("v"), false, false, true, false, false),
            RenameKeyOutcome::Edited
        ));
        assert_eq!(e.text(), "foo.txt");
        assert_eq!(e.selection(), (0, 3));
    }

    /// A ⌃-chord is NOT a clipboard chord: the guard is `platform_mod`, so ⌃V
    /// still falls through to Ignored (and pastes nothing).
    #[test]
    fn a_control_v_chord_is_still_ignored() {
        let mut e = ed("name");
        let mut cb = seeded("nope");
        assert!(matches!(
            dispatch_rename_key(&mut e, &mut cb, "v", Some("v"), false, false, false, true, false),
            RenameKeyOutcome::Ignored
        ));
        assert_eq!(e.text(), "name");
    }

    #[test]
    fn sanitize_pasted_text_flattens_controls_to_single_spaces() {
        assert_eq!(sanitize_pasted_text("foo\r\n\tbar"), "foo bar");
        // Ordinary spaces inside a segment survive verbatim.
        assert_eq!(sanitize_pasted_text("my file.txt"), "my file.txt");
        // Leading / trailing controls produce empty segments, which are dropped
        // rather than turning into stray spaces.
        assert_eq!(sanitize_pasted_text("\nfoo\n"), "foo");
        // Nothing but controls flattens to "" (the dispatch's "insert nothing").
        assert_eq!(sanitize_pasted_text("\r\n\t"), "");
        assert_eq!(sanitize_pasted_text(""), "");
    }

    #[test]
    fn escape_is_ignored_so_the_owner_binding_cancels() {
        let mut e = ed("name");
        assert!(matches!(
            dispatch_rename_key(&mut e, &mut clip(), "escape", None, false, false, false, false, false),
            RenameKeyOutcome::Ignored
        ));
        assert_eq!(e.text(), "name");
    }

    #[test]
    fn a_control_chord_is_ignored() {
        let mut e = ed("name");
        assert!(matches!(
            dispatch_rename_key(&mut e, &mut clip(), "a", Some("a"), false, false, false, true, false),
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
            dispatch_rename_key(&mut e, &mut clip(), "a", Some("a"), false, false, false, false, true),
            RenameKeyOutcome::Edited
        ));
        assert_eq!(e.text(), "A");
    }

    #[test]
    fn caps_lock_with_shift_lowercases_a_letter() {
        // Shift already made key_char "A"; Caps Lock on top cancels it back to "a".
        let mut e = ed("");
        assert!(matches!(
            dispatch_rename_key(&mut e, &mut clip(), "a", Some("A"), true, false, false, false, true),
            RenameKeyOutcome::Edited
        ));
        assert_eq!(e.text(), "a");
    }

    #[test]
    fn caps_lock_leaves_digits_and_punctuation_unchanged() {
        let mut e = ed("");
        dispatch_rename_key(&mut e, &mut clip(), "7", Some("7"), false, false, false, false, true);
        dispatch_rename_key(&mut e, &mut clip(), "-", Some("-"), false, false, false, false, true);
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
    fn field_text_carries_the_whole_string_and_the_selection() {
        // ONE string plus char offsets — the render pass shapes that string once
        // and measures the highlight from those offsets (no pre/sel/post split).
        let mut e = TextFieldEditor::with_selection("foo.txt", 3);
        let s = field_text(&e);
        assert_eq!(s.text, "foo.txt");
        assert_eq!(s.sel, (0, 3));

        e.place_cursor(2);
        let c = field_text(&e);
        assert_eq!(c.text, "foo.txt");
        assert_eq!(c.sel, (2, 2));
    }

    #[test]
    fn clicks_hit_test_against_the_paint_recorded_boundaries() {
        // The probe stands in for a painted field: text at x=100 with four 10px
        // glyph advances. Clicks round to the nearest boundary and clamp at both
        // ends; `char_boundary_x` reads the same table back.
        let probe = FieldProbe {
            field_left: 94.0,
            text_left: 100.0,
            boundaries: vec![0.0, 10.0, 20.0, 30.0, 40.0],
            ..FieldProbe::default()
        };
        assert_eq!(char_index_for_click(&probe, 100.0), 0);
        assert_eq!(char_index_for_click(&probe, 124.0), 2);
        assert_eq!(char_index_for_click(&probe, 126.0), 3);
        assert_eq!(char_index_for_click(&probe, 0.0), 0); // left of the field
        assert_eq!(char_index_for_click(&probe, 999.0), 4); // past the end
        assert_eq!(char_boundary_x(&probe, 3), Some(30.0));
        assert_eq!(char_boundary_x(&probe, 9), None);

        // An unpainted field has no table: every click is boundary 0, no panic.
        let blank = FieldProbe::default();
        assert_eq!(char_index_for_click(&blank, 42.0), 0);
        assert_eq!(char_boundary_x(&blank, 0), None);
    }
}
