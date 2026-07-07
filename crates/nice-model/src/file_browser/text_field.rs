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
            Key::SelectAll => {
                self.anchor = 0;
                self.caret = self.chars.len();
            }
        }
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

    fn collapse_to(&mut self, pos: usize) {
        self.anchor = pos;
        self.caret = pos;
    }
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
}
