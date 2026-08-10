//! `text_field` — a NEW pure editing model for the inline rename field.
//!
//! The landed [`crate::rename_gate`] click gate and R10/R11 surfaces consume
//! the char-append/backspace-only `inline_rename` field (in `crates/nice`);
//! retrofitting that field with cursor + selection mid-close-out would be
//! needless blast radius (the plan's rename-field decision). So R20 builds this
//! self-contained `{text, cursor, selection}` model — the `crates/nice` render
//! wrapper (caret, selection highlight) and the AppKit basename preselection it
//! replaces land in a later slice.
//!
//! There is no Swift twin (the Swift field is `NSTextField`-backed); this is a
//! from-scratch table-tested model. Indices are **char** offsets (`0..=len`) so
//! multi-byte names (`café.txt`) select correctly. Selection is an
//! anchor/caret pair: the anchor is the fixed end, the caret the moving end;
//! `shift`+arrow extends by moving the caret, a plain arrow collapses.
//!
//! Basename preselection ([`preselect_len`]) mirrors Finder via
//! [`split_name_and_extension`]: files with an extension select the base only
//! (so typing replaces the base and keeps `.txt`); folders, extension-less
//! files, and dotfiles select everything.

use super::naming::split_name_and_extension;

/// A key the editing model understands. Printable input is [`Key::Char`];
/// everything else is an explicit editing command. Modifiers are pre-resolved
/// by the caller into the shift/select variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A printable character — replaces the selection (or inserts at the caret).
    Char(char),
    /// Delete backward: the selection if any, else the char before the caret.
    Backspace,
    /// Delete forward: the selection if any, else the char at the caret.
    ForwardDelete,
    /// Move the caret one char left, collapsing any selection to its left edge.
    Left,
    /// Move the caret one char right, collapsing any selection to its right edge.
    Right,
    /// Extend the selection one char left (shift+left).
    ShiftLeft,
    /// Extend the selection one char right (shift+right).
    ShiftRight,
    /// Move the caret to the start of the previous word (⌥←).
    WordLeft,
    /// Move the caret to the end of the next word (⌥→).
    WordRight,
    /// Extend the selection to the start of the previous word (⌥⇧←).
    ShiftWordLeft,
    /// Extend the selection to the end of the next word (⌥⇧→).
    ShiftWordRight,
    /// Move the caret to the start of the text (⌘←; the field is single-line).
    LineStart,
    /// Move the caret to the end of the text (⌘→).
    LineEnd,
    /// Extend the selection to the start of the text (⌘⇧←).
    ShiftLineStart,
    /// Extend the selection to the end of the text (⌘⇧→).
    ShiftLineEnd,
    /// Delete the selection if any, else back to the start of the previous
    /// word (⌥⌫).
    DeleteWordBackward,
    /// Delete the selection if any, else forward to the end of the next word
    /// (⌥⌦).
    DeleteWordForward,
    /// Select the whole field (⌘A).
    SelectAll,
}

/// The pure editing state: the text plus an anchor/caret selection over char
/// offsets. `anchor == caret` is a collapsed cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextFieldEditor {
    chars: Vec<char>,
    anchor: usize,
    caret: usize,
}

impl TextFieldEditor {
    /// Seed with `text` and a collapsed caret at the end.
    pub fn new(text: &str) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let end = chars.len();
        Self {
            chars,
            anchor: end,
            caret: end,
        }
    }

    /// Seed with `text` and an initial selection `[0, len)` — the rename-field
    /// preselection. `len` is clamped to the text length.
    pub fn with_selection(text: &str, len: usize) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let len = len.min(chars.len());
        Self {
            chars,
            anchor: 0,
            caret: len,
        }
    }

    /// Current text.
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    /// The caret (active end of the selection) as a char offset.
    pub fn cursor(&self) -> usize {
        self.caret
    }

    /// The selection as an ordered `(start, end)` char-offset pair. `start ==
    /// end` means a collapsed cursor (no highlighted range).
    pub fn selection(&self) -> (usize, usize) {
        (self.anchor.min(self.caret), self.anchor.max(self.caret))
    }

    /// Whether a non-empty range is selected.
    pub fn has_selection(&self) -> bool {
        self.anchor != self.caret
    }

    /// The selected substring, or `None` when the selection is collapsed — what
    /// the ⌘C / ⌘X chords hand to the system clipboard. Char-indexed like every
    /// other offset here, so a selection over multi-byte text yields exactly the
    /// selected characters rather than a byte slice that could split one.
    ///
    /// `None` (not `Some("")`) for a collapsed caret is load-bearing: the copy
    /// chords use it to decide to leave the clipboard ALONE, which is what
    /// `NSTextField` does — a ⌘C with nothing selected must not clobber whatever
    /// the user copied a moment ago.
    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection();
        (start != end).then(|| self.chars[start..end].iter().collect())
    }

    /// Replace the selection (or insert at the caret when it is collapsed) with
    /// `s`, leaving a collapsed caret directly AFTER the inserted text — the ⌘V
    /// primitive, and the multi-char sibling of the printable [`Key::Char`]
    /// insert. One call, so the paste is one atomic edit from the key dispatch's
    /// point of view (a `Char`-per-glyph loop would work identically but makes
    /// the caret arithmetic the caller's problem).
    ///
    /// `s` is inserted verbatim; flattening a multi-line clipboard to the single
    /// line this field can hold is the caller's job (the field is single-line,
    /// but this model does not police the characters it is handed).
    pub fn insert_str(&mut self, s: &str) {
        let (start, end) = self.selection();
        let inserted: Vec<char> = s.chars().collect();
        let n = inserted.len();
        self.chars.splice(start..end, inserted);
        self.collapse_to(start + n);
    }

    /// Apply one key, mutating the state.
    pub fn apply_key(&mut self, key: Key) {
        match key {
            Key::Char(c) => self.insert(c),
            Key::Backspace => self.backspace(),
            Key::ForwardDelete => self.forward_delete(),
            Key::Left => self.move_left(false),
            Key::Right => self.move_right(false),
            Key::ShiftLeft => self.move_left(true),
            Key::ShiftRight => self.move_right(true),
            Key::WordLeft => self.move_word_left(false),
            Key::WordRight => self.move_word_right(false),
            Key::ShiftWordLeft => self.move_word_left(true),
            Key::ShiftWordRight => self.move_word_right(true),
            Key::LineStart => self.move_to_line_edge(0, false),
            Key::LineEnd => self.move_to_line_edge(self.chars.len(), false),
            Key::ShiftLineStart => self.move_to_line_edge(0, true),
            Key::ShiftLineEnd => self.move_to_line_edge(self.chars.len(), true),
            Key::DeleteWordBackward => self.delete_word_backward(),
            Key::DeleteWordForward => self.delete_word_forward(),
            Key::SelectAll => self.select_all(),
        }
    }

    /// Select the whole field — ⌘A and the triple-click "line" action (the field
    /// is single-line, so its "line" is the entire text).
    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.chars.len();
    }

    /// Select the word surrounding char boundary `pos` — the double-click action.
    /// Characters are grouped into three classes (word = alphanumeric or `_`,
    /// whitespace, other) and the contiguous run of the class under the pointer is
    /// selected, so double-clicking `bar` in `foo bar.txt` selects just `bar`. A
    /// click at the trailing edge selects the last word; an empty field leaves a
    /// collapsed caret.
    pub fn select_word_at(&mut self, pos: usize) {
        let len = self.chars.len();
        if len == 0 {
            self.collapse_to(0);
            return;
        }
        // The hit-test hands back a caret boundary in `0..=len`; the word under
        // the pointer is the run containing the char to its right (the last char
        // when the caret sits at the very end).
        let i = pos.min(len - 1);
        let class = char_class(self.chars[i]);
        let mut start = i;
        while start > 0 && char_class(self.chars[start - 1]) == class {
            start -= 1;
        }
        let mut end = i + 1;
        while end < len && char_class(self.chars[end]) == class {
            end += 1;
        }
        self.anchor = start;
        self.caret = end;
    }

    fn insert(&mut self, c: char) {
        let (start, end) = self.selection();
        self.chars.splice(start..end, std::iter::once(c));
        self.anchor = start + 1;
        self.caret = start + 1;
    }

    fn backspace(&mut self) {
        let (start, end) = self.selection();
        if start != end {
            self.chars.drain(start..end);
            self.collapse_to(start);
        } else if start > 0 {
            self.chars.remove(start - 1);
            self.collapse_to(start - 1);
        }
    }

    fn forward_delete(&mut self) {
        let (start, end) = self.selection();
        if start != end {
            self.chars.drain(start..end);
            self.collapse_to(start);
        } else if start < self.chars.len() {
            self.chars.remove(start);
            self.collapse_to(start);
        }
    }

    fn move_left(&mut self, extend: bool) {
        if extend {
            self.caret = self.caret.saturating_sub(1);
        } else if self.has_selection() {
            self.collapse_to(self.selection().0);
        } else {
            self.collapse_to(self.caret.saturating_sub(1));
        }
    }

    fn move_right(&mut self, extend: bool) {
        let len = self.chars.len();
        if extend {
            self.caret = (self.caret + 1).min(len);
        } else if self.has_selection() {
            self.collapse_to(self.selection().1);
        } else {
            self.collapse_to((self.caret + 1).min(len));
        }
    }

    /// The word-left target from char boundary `pos`: skip left over the
    /// non-word chars immediately before it, then over the word run, and land at
    /// that run's START. From the end of `"foo.bar"`: 7 → 4 → 0. Only the
    /// [`CharClass::Word`] class counts as "word" here — whitespace and
    /// punctuation are both separators for motion (NSTextField parity), unlike
    /// [`select_word_at`](Self::select_word_at) which treats each as its own run.
    fn word_left(&self, pos: usize) -> usize {
        let mut i = pos.min(self.chars.len());
        while i > 0 && char_class(self.chars[i - 1]) != CharClass::Word {
            i -= 1;
        }
        while i > 0 && char_class(self.chars[i - 1]) == CharClass::Word {
            i -= 1;
        }
        i
    }

    /// The word-right target from char boundary `pos`: skip right over the
    /// non-word chars at it, then over the word run, and land at that run's END.
    /// From the start of `"foo.bar"`: 0 → 3 → 7.
    fn word_right(&self, pos: usize) -> usize {
        let len = self.chars.len();
        let mut i = pos.min(len);
        while i < len && char_class(self.chars[i]) != CharClass::Word {
            i += 1;
        }
        while i < len && char_class(self.chars[i]) == CharClass::Word {
            i += 1;
        }
        i
    }

    /// ⌥← / ⌥⇧←. Extending moves the caret only (anchor fixed). Plain motion
    /// with a selection starts from the selection's START and then moves —
    /// macOS moves a word rather than merely collapsing (contrast plain ←).
    fn move_word_left(&mut self, extend: bool) {
        if extend {
            self.caret = self.word_left(self.caret);
        } else {
            let from = if self.has_selection() {
                self.selection().0
            } else {
                self.caret
            };
            self.collapse_to(self.word_left(from));
        }
    }

    /// ⌥→ / ⌥⇧→ — the mirror of [`move_word_left`](Self::move_word_left).
    fn move_word_right(&mut self, extend: bool) {
        if extend {
            self.caret = self.word_right(self.caret);
        } else {
            let from = if self.has_selection() {
                self.selection().1
            } else {
                self.caret
            };
            self.collapse_to(self.word_right(from));
        }
    }

    /// ⌘← / ⌘→ (+ shift). The field is single-line, so its "line" edges are `0`
    /// and `len`.
    fn move_to_line_edge(&mut self, pos: usize, extend: bool) {
        if extend {
            self.caret = pos;
        } else {
            self.collapse_to(pos);
        }
    }

    fn delete_word_backward(&mut self) {
        let (start, end) = self.selection();
        if start != end {
            self.chars.drain(start..end);
            self.collapse_to(start);
            return;
        }
        let target = self.word_left(self.caret);
        self.chars.drain(target..self.caret);
        self.collapse_to(target);
    }

    fn delete_word_forward(&mut self) {
        let (start, end) = self.selection();
        if start != end {
            self.chars.drain(start..end);
            self.collapse_to(start);
            return;
        }
        let target = self.word_right(self.caret);
        self.chars.drain(self.caret..target);
        self.collapse_to(self.caret);
    }

    /// Collapse the selection to a caret at `pos` (clamped to the text length) —
    /// the click-to-position action. The pointer picks a boundary via
    /// [`char_index_for_x`]; this drops the caret there and clears any selection,
    /// so a subsequent keystroke inserts at the click point rather than replacing
    /// the whole field.
    pub fn place_cursor(&mut self, pos: usize) {
        self.collapse_to(pos.min(self.chars.len()));
    }

    /// Move the caret to `pos` (clamped) while KEEPING the anchor — the
    /// extend-selection primitive. This is what a click-drag calls on every
    /// mouse-move after the press placed the anchor, and what a shift-click
    /// would call; unlike [`place_cursor`](Self::place_cursor) it never
    /// collapses.
    pub fn extend_to(&mut self, pos: usize) {
        self.caret = pos.min(self.chars.len());
    }

    fn collapse_to(&mut self, pos: usize) {
        self.anchor = pos;
        self.caret = pos;
    }
}

/// The three character classes [`TextFieldEditor::select_word_at`] groups by: a
/// double-click expands to the contiguous run of the class under the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    /// Alphanumeric or `_` — the letters/digits that make up a word.
    Word,
    /// Any Unicode whitespace.
    Whitespace,
    /// Everything else (punctuation, symbols) — grouped as a run, so `...`
    /// selects together.
    Other,
}

fn char_class(c: char) -> CharClass {
    if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else if c.is_whitespace() {
        CharClass::Whitespace
    } else {
        CharClass::Other
    }
}

/// Map a click x-offset (pixels from the text's left edge) to the nearest
/// character-boundary index.
///
/// `boundaries[i]` is the x-position of boundary `i`: for a field of `n`
/// characters there are `n + 1` boundaries, with `boundaries[0] == 0.0` (before
/// the first char) and `boundaries[n]` the full line width (after the last).
/// Returns the index whose boundary is closest to `x`. A click past the trailing
/// edge clamps to `n` (caret at the end); a click before the start clamps to `0`.
/// Ties round to the earlier (left) boundary.
///
/// Pure and font-agnostic: the caller measures the glyph advances (via the
/// window's text system) and hands the resulting boundary table in, so the
/// rounding rule is unit-tested without a window.
pub fn char_index_for_x(boundaries: &[f32], x: f32) -> usize {
    if boundaries.is_empty() {
        return 0;
    }
    let mut best = 0;
    let mut best_dist = (boundaries[0] - x).abs();
    for (i, &b) in boundaries.iter().enumerate().skip(1) {
        let d = (b - x).abs();
        if d < best_dist {
            best = i;
            best_dist = d;
        }
    }
    best
}

/// The basename-preselection length for `name`: the base length for a
/// non-directory file WITH an extension, else the whole-name length. Dotfiles
/// have no extension (per [`split_name_and_extension`]) so they select
/// everything — matching Finder and `FileBrowserView.swift:928-937`.
pub fn preselect_len(name: &str, is_directory: bool) -> usize {
    let (base, ext) = split_name_and_extension(name);
    if !ext.is_empty() && !is_directory {
        base.chars().count()
    } else {
        name.chars().count()
    }
}

#[cfg(test)]
mod tests {
    use super::Key::*;
    use super::*;

    fn apply(editor: &mut TextFieldEditor, keys: &[Key]) {
        for &k in keys {
            editor.apply_key(k);
        }
    }

    #[test]
    fn new_places_collapsed_caret_at_end() {
        let e = TextFieldEditor::new("foo");
        assert_eq!(e.text(), "foo");
        assert_eq!(e.cursor(), 3);
        assert_eq!(e.selection(), (3, 3));
        assert!(!e.has_selection());
    }

    #[test]
    fn with_selection_preselects_range() {
        let e = TextFieldEditor::with_selection("foo.txt", 3);
        assert_eq!(e.selection(), (0, 3));
        assert_eq!(e.cursor(), 3);
        assert!(e.has_selection());
    }

    #[test]
    fn with_selection_clamps_overlong_len() {
        let e = TextFieldEditor::with_selection("hi", 99);
        assert_eq!(e.selection(), (0, 2));
    }

    #[test]
    fn with_selection_whole_title_selects_all() {
        // The session/window-pill rename seed: passing the char count selects the WHOLE
        // title (not base-minus-extension), so the first keystroke replaces it.
        let title = "my session tab";
        let e = TextFieldEditor::with_selection(title, title.chars().count());
        assert_eq!(e.selection(), (0, title.chars().count()));
        assert!(e.has_selection());
    }

    #[test]
    fn printable_replaces_selection() {
        // Preselect "foo" of "foo.txt", type 'b' → "b.txt".
        let mut e = TextFieldEditor::with_selection("foo.txt", 3);
        e.apply_key(Char('b'));
        assert_eq!(e.text(), "b.txt");
        assert_eq!(e.selection(), (1, 1));
    }

    #[test]
    fn printable_inserts_at_collapsed_caret() {
        let mut e = TextFieldEditor::new("ac");
        e.apply_key(Left); // caret between a and c
        e.apply_key(Char('b'));
        assert_eq!(e.text(), "abc");
        assert_eq!(e.cursor(), 2);
    }

    #[test]
    fn backspace_deletes_char_before_caret() {
        let mut e = TextFieldEditor::new("foo");
        e.apply_key(Backspace);
        assert_eq!(e.text(), "fo");
        assert_eq!(e.cursor(), 2);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut e = TextFieldEditor::new("foo");
        apply(&mut e, &[Left, Left, Left, Backspace]);
        assert_eq!(e.text(), "foo");
        assert_eq!(e.cursor(), 0);
    }

    #[test]
    fn backspace_deletes_selection() {
        let mut e = TextFieldEditor::with_selection("foo.txt", 3);
        e.apply_key(Backspace);
        assert_eq!(e.text(), ".txt");
        assert_eq!(e.selection(), (0, 0));
    }

    #[test]
    fn forward_delete_removes_char_at_caret() {
        let mut e = TextFieldEditor::new("foo");
        apply(&mut e, &[Left, Left, ForwardDelete]);
        assert_eq!(e.text(), "fo");
        assert_eq!(e.cursor(), 1);
    }

    #[test]
    fn forward_delete_at_end_is_noop() {
        let mut e = TextFieldEditor::new("foo");
        e.apply_key(ForwardDelete);
        assert_eq!(e.text(), "foo");
    }

    #[test]
    fn forward_delete_removes_selection() {
        let mut e = TextFieldEditor::with_selection("foo.txt", 3);
        e.apply_key(ForwardDelete);
        assert_eq!(e.text(), ".txt");
    }

    #[test]
    fn left_and_right_collapse_selection_to_edges() {
        let mut e = TextFieldEditor::with_selection("foobar", 6);
        e.apply_key(Left);
        assert_eq!(e.cursor(), 0);
        assert!(!e.has_selection());

        let mut e2 = TextFieldEditor::with_selection("foobar", 6);
        e2.apply_key(Right);
        assert_eq!(e2.cursor(), 6);
        assert!(!e2.has_selection());
    }

    #[test]
    fn left_right_clamp_at_bounds() {
        let mut e = TextFieldEditor::new("ab");
        apply(&mut e, &[Left, Left, Left]);
        assert_eq!(e.cursor(), 0);
        apply(&mut e, &[Right, Right, Right]);
        assert_eq!(e.cursor(), 2);
    }

    #[test]
    fn shift_arrows_extend_selection() {
        let mut e = TextFieldEditor::new("foo"); // caret at 3
        e.apply_key(ShiftLeft);
        assert_eq!(e.selection(), (2, 3));
        e.apply_key(ShiftLeft);
        assert_eq!(e.selection(), (1, 3));
        // Shrink back with shift-right.
        e.apply_key(ShiftRight);
        assert_eq!(e.selection(), (2, 3));
    }

    #[test]
    fn select_all_selects_whole_field() {
        let mut e = TextFieldEditor::new("hello");
        e.apply_key(SelectAll);
        assert_eq!(e.selection(), (0, 5));
    }

    #[test]
    fn multibyte_selection_is_char_indexed() {
        // "café" preselected as base of "café.txt".
        let mut e = TextFieldEditor::with_selection("café.txt", preselect_len("café.txt", false));
        assert_eq!(e.selection(), (0, 4));
        e.apply_key(Char('x'));
        assert_eq!(e.text(), "x.txt");
    }

    // MARK: - selected_text / insert_str (the ⌘C/⌘X/⌘V primitives)

    #[test]
    fn selected_text_is_none_when_the_selection_is_collapsed() {
        // The copy chords read this to decide NOT to touch the clipboard.
        let e = TextFieldEditor::new("foo.txt");
        assert!(!e.has_selection());
        assert_eq!(e.selected_text(), None);
    }

    #[test]
    fn selected_text_returns_the_selected_substring() {
        let e = TextFieldEditor::with_selection("foo.txt", 3);
        assert_eq!(e.selected_text().as_deref(), Some("foo"));
    }

    #[test]
    fn selected_text_is_char_indexed_for_multibyte() {
        // Offsets are char offsets: selecting the two-char "åØ" out of "xåØy"
        // must not split either two-byte glyph.
        let mut e = TextFieldEditor::new("xåØy");
        e.place_cursor(1);
        e.extend_to(3);
        assert_eq!(e.selected_text().as_deref(), Some("åØ"));
    }

    #[test]
    fn insert_str_replaces_the_selection_and_leaves_the_caret_after_it() {
        // Preselect "foo" of "foo.txt" and paste "bar" over it.
        let mut e = TextFieldEditor::with_selection("foo.txt", 3);
        e.insert_str("bar");
        assert_eq!(e.text(), "bar.txt");
        assert_eq!(e.cursor(), 3);
        assert_eq!(e.selection(), (3, 3));
        assert!(!e.has_selection());
    }

    #[test]
    fn insert_str_inserts_at_a_collapsed_caret_mid_text() {
        let mut e = TextFieldEditor::new("ad");
        e.place_cursor(1);
        e.insert_str("bc");
        assert_eq!(e.text(), "abcd");
        assert_eq!(e.cursor(), 3);
    }

    #[test]
    fn insert_str_is_char_indexed_for_multibyte() {
        // The caret lands after TWO chars, not after four bytes.
        let mut e = TextFieldEditor::new("ab");
        e.place_cursor(1);
        e.insert_str("åØ");
        assert_eq!(e.text(), "aåØb");
        assert_eq!(e.cursor(), 3);
    }

    // MARK: - word motion (⌥←/⌥→)

    #[test]
    fn word_left_lands_on_word_run_starts() {
        // "foo.bar", caret at the end: 7 → 4 ("bar" start) → 0 ("foo" start).
        let mut e = TextFieldEditor::new("foo.bar");
        e.apply_key(WordLeft);
        assert_eq!(e.cursor(), 4);
        e.apply_key(WordLeft);
        assert_eq!(e.cursor(), 0);
        // Clamped at the start.
        e.apply_key(WordLeft);
        assert_eq!(e.cursor(), 0);
        assert!(!e.has_selection());
    }

    #[test]
    fn word_right_lands_on_word_run_ends() {
        // "foo.bar" from 0: → 3 ("foo" end) → 7 ("bar" end).
        let mut e = TextFieldEditor::new("foo.bar");
        e.place_cursor(0);
        e.apply_key(WordRight);
        assert_eq!(e.cursor(), 3);
        e.apply_key(WordRight);
        assert_eq!(e.cursor(), 7);
        e.apply_key(WordRight);
        assert_eq!(e.cursor(), 7);
        assert!(!e.has_selection());
    }

    #[test]
    fn word_motion_treats_whitespace_and_punctuation_alike() {
        // Motion classes are word / not-word, so "alpha beta_gamma.txt" walks
        // its word runs regardless of which separator sits between them.
        let mut e = TextFieldEditor::new("alpha beta_gamma.txt");
        assert_eq!(e.cursor(), 20);
        e.apply_key(WordLeft);
        assert_eq!(e.cursor(), 17); // start of "txt"
        e.apply_key(WordLeft);
        assert_eq!(e.cursor(), 6); // start of "beta_gamma" (underscore is a word char)
        e.apply_key(WordLeft);
        assert_eq!(e.cursor(), 0); // start of "alpha"
    }

    #[test]
    fn plain_word_motion_moves_from_the_selection_edge() {
        // macOS moves a whole word out of a selection (unlike plain ←/→, which
        // only collapse). "foo.bar" fully selected: ⌥← from start 0 → 0,
        // ⌥→ from end 7 → 7; use an interior selection to see the move.
        let mut e = TextFieldEditor::new("foo.bar");
        e.place_cursor(4);
        e.apply_key(ShiftRight); // selection (4,5)
        assert_eq!(e.selection(), (4, 5));
        e.apply_key(WordLeft); // from selection start 4 → 0
        assert_eq!(e.cursor(), 0);
        assert!(!e.has_selection());

        let mut e2 = TextFieldEditor::new("foo.bar");
        e2.place_cursor(1);
        e2.apply_key(ShiftRight); // selection (1,2)
        e2.apply_key(WordRight); // from selection end 2 → 3
        assert_eq!(e2.cursor(), 3);
        assert!(!e2.has_selection());
    }

    #[test]
    fn shift_word_motion_extends_with_a_fixed_anchor() {
        let mut e = TextFieldEditor::new("foo.bar"); // caret at 7
        e.apply_key(ShiftWordLeft);
        assert_eq!(e.selection(), (4, 7));
        e.apply_key(ShiftWordLeft);
        assert_eq!(e.selection(), (0, 7));
        // Shrinking back the other way keeps the anchor at 7.
        e.apply_key(ShiftWordRight);
        assert_eq!(e.selection(), (3, 7));
    }

    #[test]
    fn word_motion_is_char_indexed_for_multibyte() {
        let mut e = TextFieldEditor::new("café bar");
        e.place_cursor(0);
        e.apply_key(WordRight);
        assert_eq!(e.cursor(), 4); // after "café", not a byte offset
    }

    // MARK: - line motion (⌘←/⌘→)

    #[test]
    fn line_start_and_end_jump_to_the_text_edges() {
        let mut e = TextFieldEditor::new("foo.bar");
        e.apply_key(LineStart);
        assert_eq!(e.cursor(), 0);
        assert!(!e.has_selection());
        e.apply_key(LineEnd);
        assert_eq!(e.cursor(), 7);
        assert!(!e.has_selection());
    }

    #[test]
    fn line_motion_collapses_an_existing_selection() {
        let mut e = TextFieldEditor::with_selection("foo.bar", 3);
        e.apply_key(LineEnd);
        assert_eq!(e.selection(), (7, 7));
    }

    #[test]
    fn shift_line_motion_extends_to_the_edges() {
        let mut e = TextFieldEditor::new("foo.bar");
        e.place_cursor(3);
        e.apply_key(ShiftLineStart);
        assert_eq!(e.selection(), (0, 3));
        assert_eq!(e.cursor(), 0);

        let mut e2 = TextFieldEditor::new("foo.bar");
        e2.place_cursor(3);
        e2.apply_key(ShiftLineEnd);
        assert_eq!(e2.selection(), (3, 7));
        assert_eq!(e2.cursor(), 7);
    }

    // MARK: - word delete (⌥⌫ / ⌥⌦)

    #[test]
    fn delete_word_backward_removes_the_previous_word() {
        let mut e = TextFieldEditor::new("foo.bar");
        e.apply_key(DeleteWordBackward);
        assert_eq!(e.text(), "foo.");
        assert_eq!(e.cursor(), 4);
        e.apply_key(DeleteWordBackward);
        assert_eq!(e.text(), "");
        assert_eq!(e.cursor(), 0);
        // At the start it is a no-op.
        e.apply_key(DeleteWordBackward);
        assert_eq!(e.text(), "");
    }

    #[test]
    fn delete_word_backward_removes_the_selection_when_there_is_one() {
        let mut e = TextFieldEditor::with_selection("foo.txt", 3);
        e.apply_key(DeleteWordBackward);
        assert_eq!(e.text(), ".txt");
        assert_eq!(e.selection(), (0, 0));
    }

    #[test]
    fn delete_word_forward_removes_the_next_word() {
        let mut e = TextFieldEditor::new("foo.bar");
        e.place_cursor(0);
        e.apply_key(DeleteWordForward);
        assert_eq!(e.text(), ".bar");
        assert_eq!(e.cursor(), 0);
        e.apply_key(DeleteWordForward);
        assert_eq!(e.text(), "");
        assert_eq!(e.cursor(), 0);
        e.apply_key(DeleteWordForward);
        assert_eq!(e.text(), "");
    }

    #[test]
    fn delete_word_forward_removes_the_selection_when_there_is_one() {
        let mut e = TextFieldEditor::with_selection("foo.txt", 3);
        e.apply_key(DeleteWordForward);
        assert_eq!(e.text(), ".txt");
        assert_eq!(e.selection(), (0, 0));
    }

    #[test]
    fn delete_word_backward_is_char_indexed_for_multibyte() {
        let mut e = TextFieldEditor::new("café bar");
        e.apply_key(DeleteWordBackward);
        assert_eq!(e.text(), "café ");
    }

    // MARK: - extend_to (drag-select)

    #[test]
    fn extend_to_moves_the_caret_and_keeps_the_anchor() {
        let mut e = TextFieldEditor::new("foo bar baz");
        e.place_cursor(2); // anchor + caret at 2
        e.extend_to(6);
        assert_eq!(e.selection(), (2, 6));
        assert_eq!(e.cursor(), 6);
        assert!(e.has_selection());
        // Dragging back past the anchor flips the ordered range, anchor fixed.
        e.extend_to(0);
        assert_eq!(e.selection(), (0, 2));
        assert_eq!(e.cursor(), 0);
    }

    #[test]
    fn extend_to_clamps_past_the_end() {
        let mut e = TextFieldEditor::new("abc");
        e.place_cursor(1);
        e.extend_to(99);
        assert_eq!(e.selection(), (1, 3));
    }

    #[test]
    fn extend_to_back_onto_the_anchor_collapses_the_selection() {
        let mut e = TextFieldEditor::new("abc");
        e.place_cursor(1);
        e.extend_to(3);
        e.extend_to(1);
        assert_eq!(e.selection(), (1, 1));
        assert!(!e.has_selection());
    }

    #[test]
    fn extend_to_then_type_replaces_the_dragged_range() {
        let mut e = TextFieldEditor::new("foo bar");
        e.place_cursor(0);
        e.extend_to(3);
        e.apply_key(Char('X'));
        assert_eq!(e.text(), "X bar");
    }

    // MARK: - preselect_len

    #[test]
    fn preselect_len_file_with_extension_selects_base() {
        assert_eq!(preselect_len("foo.txt", false), 3);
    }

    #[test]
    fn preselect_len_directory_selects_all() {
        assert_eq!(preselect_len("foo.txt", true), 7);
    }

    #[test]
    fn preselect_len_extensionless_selects_all() {
        assert_eq!(preselect_len("README", false), 6);
    }

    #[test]
    fn preselect_len_dotfile_selects_all() {
        // `.zshrc` is whole-name base (no extension) → select everything.
        assert_eq!(preselect_len(".zshrc", false), 6);
    }

    // MARK: - place_cursor

    #[test]
    fn place_cursor_drops_a_collapsed_caret_and_clears_selection() {
        let mut e = TextFieldEditor::with_selection("foo.txt", 3); // [0,3) selected
        e.place_cursor(2);
        assert_eq!(e.cursor(), 2);
        assert_eq!(e.selection(), (2, 2));
        assert!(!e.has_selection());
    }

    #[test]
    fn place_cursor_clamps_past_the_end() {
        let mut e = TextFieldEditor::new("abc");
        e.place_cursor(99);
        assert_eq!(e.cursor(), 3);
        assert_eq!(e.selection(), (3, 3));
    }

    #[test]
    fn place_cursor_then_type_inserts_at_the_click_point() {
        // Click between 'a' and 'b' of "abc", then type 'X' → "aXbc".
        let mut e = TextFieldEditor::new("abc");
        e.place_cursor(1);
        e.apply_key(Char('X'));
        assert_eq!(e.text(), "aXbc");
        assert_eq!(e.cursor(), 2);
    }

    // MARK: - select_word_at (double-click)

    #[test]
    fn select_word_at_selects_the_word_run() {
        // Click inside "bar" of "foo bar.txt".
        let mut e = TextFieldEditor::new("foo bar.txt");
        e.select_word_at(5); // caret between 'b' and 'a'
        assert_eq!(e.selection(), (4, 7)); // "bar"
    }

    #[test]
    fn select_word_at_selects_the_first_word() {
        let mut e = TextFieldEditor::new("foo bar");
        e.select_word_at(1);
        assert_eq!(e.selection(), (0, 3)); // "foo"
    }

    #[test]
    fn select_word_at_end_selects_the_last_word() {
        // Caret at the very end clamps onto the last char's run.
        let mut e = TextFieldEditor::new("foo bar");
        e.select_word_at(7);
        assert_eq!(e.selection(), (4, 7)); // "bar"
    }

    #[test]
    fn select_word_at_groups_whitespace() {
        let mut e = TextFieldEditor::new("a   b");
        e.select_word_at(2); // inside the run of spaces
        assert_eq!(e.selection(), (1, 4));
    }

    #[test]
    fn select_word_at_groups_punctuation() {
        // The '.' between base and ext is its own "other" run.
        let mut e = TextFieldEditor::new("foo...bar");
        e.select_word_at(4); // inside "..."
        assert_eq!(e.selection(), (3, 6));
    }

    #[test]
    fn select_word_at_underscore_is_part_of_the_word() {
        let mut e = TextFieldEditor::new("my_tab_name");
        e.select_word_at(4);
        assert_eq!(e.selection(), (0, 11)); // whole snake_case token
    }

    #[test]
    fn select_word_at_multibyte_is_char_indexed() {
        let mut e = TextFieldEditor::new("café bar");
        e.select_word_at(2);
        assert_eq!(e.selection(), (0, 4)); // "café"
    }

    #[test]
    fn select_word_at_empty_field_is_a_collapsed_caret() {
        let mut e = TextFieldEditor::new("");
        e.select_word_at(0);
        assert_eq!(e.selection(), (0, 0));
        assert!(!e.has_selection());
    }

    // MARK: - select_all (triple-click / ⌘A)

    #[test]
    fn select_all_method_selects_whole_field() {
        let mut e = TextFieldEditor::new("foo bar");
        e.select_all();
        assert_eq!(e.selection(), (0, 7));
    }

    // MARK: - char_index_for_x

    /// Boundaries for a 3-char monospace-ish line at 10px/char: [0,10,20,30].
    const B: &[f32] = &[0.0, 10.0, 20.0, 30.0];

    #[test]
    fn char_index_for_x_snaps_to_nearest_boundary() {
        assert_eq!(char_index_for_x(B, 0.0), 0);
        assert_eq!(char_index_for_x(B, 4.0), 0); // closer to 0 than 10
        assert_eq!(char_index_for_x(B, 6.0), 1); // closer to 10 than 0
        assert_eq!(char_index_for_x(B, 14.0), 1);
        assert_eq!(char_index_for_x(B, 16.0), 2);
    }

    #[test]
    fn char_index_for_x_past_the_end_clamps_to_last() {
        assert_eq!(char_index_for_x(B, 100.0), 3);
    }

    #[test]
    fn char_index_for_x_before_the_start_clamps_to_zero() {
        assert_eq!(char_index_for_x(B, -50.0), 0);
    }

    #[test]
    fn char_index_for_x_midpoint_ties_round_left() {
        // Exactly halfway between boundary 0 (0.0) and boundary 1 (10.0).
        assert_eq!(char_index_for_x(B, 5.0), 0);
    }

    /// The half-glyph convention: `boundaries[i]` is the LEFT edge of char `i`
    /// (`boundaries[0] == 0.0`, final entry = end of text), so a click on the
    /// LEFT half of glyph `i` puts the caret BEFORE it (index `i`) and a click
    /// on its RIGHT half puts the caret AFTER it (index `i + 1`).
    #[test]
    fn char_index_for_x_left_half_before_right_half_after() {
        // Glyph 1 spans [10, 20): left half → caret 1, right half → caret 2.
        assert_eq!(char_index_for_x(B, 12.0), 1, "left half of glyph 1 → before it");
        assert_eq!(char_index_for_x(B, 18.0), 2, "right half of glyph 1 → after it");
        // And glyph 0 spans [0, 10): its left half is caret 0, never 1.
        assert_eq!(char_index_for_x(B, 2.0), 0, "left half of glyph 0 → caret 0");
    }

    #[test]
    fn char_index_for_x_empty_boundaries_is_zero() {
        assert_eq!(char_index_for_x(&[], 12.0), 0);
    }

    #[test]
    fn char_index_for_x_handles_uneven_advances() {
        // Proportional font: a narrow 'i' then a wide 'W'. Boundaries [0,4,20].
        let b = &[0.0, 4.0, 20.0];
        assert_eq!(char_index_for_x(b, 1.0), 0);
        assert_eq!(char_index_for_x(b, 3.0), 1); // 3 is closer to 4 than to 0
        assert_eq!(char_index_for_x(b, 11.0), 1); // closer to 4 than to 20
        assert_eq!(char_index_for_x(b, 13.0), 2); // closer to 20 than to 4
    }
}
