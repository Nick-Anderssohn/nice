//! The per-pane scrollback-search field (tmux-port Phase 3, P2) — the query half
//! of copy mode.
//!
//! ## Why it lives in the app crate
//!
//! The search ENGINE is on `nice_term_view::TerminalSessionHandle` (matches,
//! navigation, the compiled regex). The FIELD cannot be: `nice-term-view` has no
//! `nice-model` dependency by doctrine, and the only text editor this codebase
//! owns is `nice_model::file_browser::TextFieldEditor`. So the bar is app-side,
//! it pushes each keystroke's query down through the handle, and the two halves
//! meet at that API — exactly the split P2 specifies.
//!
//! ## It is the inline-rename field, re-dressed
//!
//! There is no native gpui text input in this codebase; [`crate::inline_rename`]
//! is the one hand-painted field, already carrying the caret, the selection
//! highlight, click hit-testing, drag-select and ⌘C/⌘X/⌘V. This module reuses it
//! whole rather than growing a second editor: [`dispatch_search_key`] is
//! [`dispatch_rename_key`] plus the two verbs a search bar has and a rename does
//! not — Enter CONFIRMS a search (it does not commit a name) and Escape CLOSES
//! the bar (the rename field deliberately leaves Escape to its owner, and here
//! the owner is this bar).
//!
//! ## Who owns what
//!
//! [`SearchBarState`] hangs off `WindowState` (one per window, keyed to the pane
//! it was opened on) so an App-level action handler can open it with no
//! `&mut Window` in hand. `WindowHostView` renders it as an absolute child of
//! that pane's wrapper and owns the key/click listeners, because it is the view
//! that can move key focus back to the terminal when the bar closes.

use gpui::{div, px, App, FocusHandle, IntoElement, KeyDownEvent, ParentElement, Styled, Window};

use nice_model::file_browser::TextFieldEditor;

use crate::inline_rename::{
    dispatch_rename_key, field_probe_cell, field_text, rename_field, FieldColors, FieldProbeCell,
    FieldText, RenameClipboard, RenameKeyOutcome,
};

/// The key context the bar's root carries. Distinct from the rename fields'
/// (`PaneRename` / `SessionRename` / …) so a future keymap predicate can target
/// the search field alone.
pub(crate) const SEARCH_BAR_KEY_CONTEXT: &str = "PaneSearch";

/// The bar's width in px. Wide enough for a realistic query, narrow enough that
/// it never covers the pane it is searching.
pub(crate) const SEARCH_BAR_WIDTH: f32 = 260.0;

/// The bar's text size in px — the chrome scale, not the terminal's.
pub(crate) const SEARCH_BAR_TEXT_SIZE: f32 = 12.0;

/// The gap between the bar and its pane's bottom-right corner, in px. Clear of
/// the Phase-2 corner ticks.
const SEARCH_BAR_INSET: f32 = 14.0;

/// The vi prompt glyph shown ahead of the query: `?` for a backward search,
/// `/` for a forward one (P7's directions, spelled the way vim spells them).
pub(crate) fn search_prompt_glyph(backward: bool) -> &'static str {
    if backward {
        "?"
    } else {
        "/"
    }
}

/// The open search field: its editor, its focus, the direction it searches, and
/// the PANE it belongs to.
///
/// The pane key is carried whole (`session`, `term_window`, `pane`) because the
/// bar drives one specific pane's handle: copy mode and search are per-pane
/// (Phase 3's vocabulary note), and the focused pane can change under an open
/// bar — which is one of the three conditions that close it (see
/// [`search_bar_is_stale`]).
pub(crate) struct SearchBarState {
    /// The query text being edited.
    pub(crate) editor: TextFieldEditor,
    /// The field's own focus handle — while the bar is focused, gpui routes keys
    /// here and the pane's `TerminalView` sees nothing (P3).
    pub(crate) focus: FocusHandle,
    /// Whether this search runs toward history (P7: `⌃⌘/` and in-mode `?` do,
    /// in-mode `/` does not).
    pub(crate) backward: bool,
    /// The session owning the searched pane.
    pub(crate) session_id: String,
    /// The pill owning the searched pane.
    pub(crate) term_window_id: String,
    /// The searched pane.
    pub(crate) pane_id: String,
    /// The field's paint→mouse channel (caret geometry + the live drag-select),
    /// see [`crate::inline_rename::FieldProbe`].
    pub(crate) probe: FieldProbeCell,
}

impl SearchBarState {
    /// A fresh, empty bar over `pane`, searching in `backward`'s direction.
    pub(crate) fn new(
        focus: FocusHandle,
        backward: bool,
        session_id: impl Into<String>,
        term_window_id: impl Into<String>,
        pane_id: impl Into<String>,
    ) -> Self {
        Self {
            editor: TextFieldEditor::new(""),
            focus,
            backward,
            session_id: session_id.into(),
            term_window_id: term_window_id.into(),
            pane_id: pane_id.into(),
            probe: field_probe_cell(),
        }
    }

    /// The current query text — what gets pushed down to the pane's handle on
    /// every edit (P8's incremental highlighting).
    pub(crate) fn query(&self) -> String {
        self.editor.text()
    }

    /// Whether this bar belongs to `pane_id`.
    pub(crate) fn targets_pane(&self, pane_id: &str) -> bool {
        self.pane_id == pane_id
    }

    /// The bar's pane key, for the handle lookups its owners perform.
    pub(crate) fn pane_key(&self) -> (&str, &str, &str) {
        (&self.session_id, &self.term_window_id, &self.pane_id)
    }

    /// Everything one paint of the bar needs, OWNED. The render path reads it
    /// out of the window state and drops that borrow before it starts building
    /// listeners over the same entity — the field cannot be handed a reference
    /// into the state it is going to mutate.
    pub(crate) fn paint_snapshot(&self) -> SearchBarPaint {
        SearchBarPaint {
            focus: self.focus.clone(),
            text: field_text(&self.editor),
            backward: self.backward,
            probe: self.probe.clone(),
        }
    }
}

/// One paint's worth of [`SearchBarState`] — see
/// [`paint_snapshot`](SearchBarState::paint_snapshot).
pub(crate) struct SearchBarPaint {
    /// The field's focus handle (`track_focus`).
    pub(crate) focus: FocusHandle,
    /// The query text plus its selection / caret.
    pub(crate) text: FieldText,
    /// Which prompt glyph to draw.
    pub(crate) backward: bool,
    /// The field's paint→mouse channel.
    pub(crate) probe: FieldProbeCell,
}

/// What feeding one keystroke to the open search bar should do.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SearchKeyOutcome {
    /// Enter — jump to the nearest match, close the bar, stay in copy mode (P7).
    Confirm,
    /// Escape — close the bar and leave the pane in copy mode at the position it
    /// is already at (P7). The query's highlights go with the bar.
    Close,
    /// The editor changed: re-push the query and repaint.
    Edited,
    /// A key the field does not claim; let it propagate.
    Ignored,
}

/// Apply one keystroke to the open search bar. Pure over the editor plus the
/// injected clipboard, exactly like [`dispatch_rename_key`] — which does all the
/// editing work here; this adds only the two verbs a search field has that a
/// rename field does not:
///
/// * Enter → [`SearchKeyOutcome::Confirm`] (the rename field's `Commit`, renamed
///   for what it means here),
/// * Escape → [`SearchKeyOutcome::Close`]. The rename field leaves Escape
///   `Ignored` on purpose ("the owner cancels"), and for this field the owner IS
///   this dispatch: nothing else would close the bar. It is claimed only as a
///   BARE Escape, so ⌘⎋ and friends still propagate.
///
/// One subtraction as well: a chord holding BOTH ⌘ and ⌃ is never the field's.
/// The rename field's ⌘C/⌘X/⌘V arms gate on the platform modifier alone, so ⌃⌘C
/// would read as a copy — and ⌃⌘C is how the user leaves copy mode, which is
/// exactly the chord that must survive a focused search field. Every rung of
/// Nice's shortcut ladder lives on ⌃⌘, so the whole set is refused here rather
/// than the one chord.
pub(crate) fn dispatch_search_key(
    editor: &mut TextFieldEditor,
    clipboard: &mut impl RenameClipboard,
    key: &str,
    key_char: Option<&str>,
    shift: bool,
    alt: bool,
    platform_mod: bool,
    control_mod: bool,
    capslock: bool,
) -> SearchKeyOutcome {
    if platform_mod && control_mod {
        return SearchKeyOutcome::Ignored;
    }
    if key == "escape" && !platform_mod && !control_mod && !alt {
        return SearchKeyOutcome::Close;
    }
    match dispatch_rename_key(
        editor,
        clipboard,
        key,
        key_char,
        shift,
        alt,
        platform_mod,
        control_mod,
        capslock,
    ) {
        RenameKeyOutcome::Commit => SearchKeyOutcome::Confirm,
        RenameKeyOutcome::Edited => SearchKeyOutcome::Edited,
        RenameKeyOutcome::Ignored => SearchKeyOutcome::Ignored,
    }
}

/// Whether an open bar has gone stale and must close itself, checked on every
/// render (the Phase-2 divider-drag rule: a stale key is a close, never a panic).
/// Three ways it goes stale, all of them reachable by a user doing nothing wrong:
///
/// * its pane is gone (`focused_pane` is `None`, or the pane died and the pill
///   now focuses another leaf),
/// * the focused pane changed — a bar over a pane the keyboard has left would
///   drive a terminal the user is no longer looking at,
/// * **its pane left copy mode.** `⌃⌘c` is a global action that fires even while
///   the bar holds focus, and `Esc`/`q`/`y` can end the mode after a click back
///   into the pane. Without this check the bar would sit there showing a query
///   whose state P6 has already cleared.
pub(crate) fn search_bar_is_stale(
    bar_pane: &str,
    focused_pane: Option<&str>,
    copy_mode_active: bool,
) -> bool {
    focused_pane != Some(bar_pane) || !copy_mode_active
}

/// The bar element: the direction glyph plus the shared inline-rename field,
/// anchored at the bottom-right of the pane it searches.
///
/// It is `absolute()`, so its parent must be the pane wrapper's `relative()` box
/// (`WindowHostView::leaf_element`) — the same parentage the Phase-2 corner ticks
/// use, which is what makes zoom, divider drags, window resizes and tree
/// restructures place it correctly for free.
pub(crate) fn search_bar_element(
    bar: SearchBarPaint,
    colors: FieldColors,
    on_key: impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static,
    on_click_index: impl Fn(usize, usize, &mut Window, &mut App) + 'static,
    on_drag_index: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let field = rename_field(
        &bar.focus,
        &bar.text,
        SEARCH_BAR_KEY_CONTEXT,
        colors,
        SEARCH_BAR_TEXT_SIZE,
        bar.probe,
        on_key,
        on_click_index,
        on_drag_index,
    );
    div()
        .absolute()
        .bottom(px(SEARCH_BAR_INSET))
        .right(px(SEARCH_BAR_INSET))
        .w(px(SEARCH_BAR_WIDTH))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .child(
            div()
                .text_size(px(SEARCH_BAR_TEXT_SIZE))
                .text_color(colors.text)
                .child(search_prompt_glyph(bar.backward)),
        )
        .child(field)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The in-memory clipboard the dispatch tests pass (the rename field's own
    /// fake, re-declared here so this module's tests need no window either).
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

    /// `key, key_char` then the modifier tail: `shift, alt, platform, control,
    /// capslock`.
    fn press(editor: &mut TextFieldEditor, key: &str, key_char: Option<&str>) -> SearchKeyOutcome {
        dispatch_search_key(
            editor,
            &mut FakeClipboard::default(),
            key,
            key_char,
            false,
            false,
            false,
            false,
            false,
        )
    }

    #[test]
    fn typing_builds_the_query() {
        let mut editor = TextFieldEditor::new("");
        for ch in ["n", "e", "e"] {
            assert_eq!(press(&mut editor, ch, Some(ch)), SearchKeyOutcome::Edited);
        }
        assert_eq!(editor.text(), "nee");
        // Backspace edits it, like any field.
        assert_eq!(press(&mut editor, "backspace", None), SearchKeyOutcome::Edited);
        assert_eq!(editor.text(), "ne");
    }

    #[test]
    fn enter_confirms_and_escape_closes() {
        let mut editor = TextFieldEditor::new("needle");
        assert_eq!(press(&mut editor, "enter", None), SearchKeyOutcome::Confirm);
        // Neither verb edits the query — confirming searches for what is typed,
        // and closing leaves the pane exactly where it stands (P7).
        assert_eq!(editor.text(), "needle");
        assert_eq!(press(&mut editor, "escape", None), SearchKeyOutcome::Close);
        assert_eq!(editor.text(), "needle");
    }

    /// Escape is claimed only BARE: a modified Escape still propagates, so the
    /// bar can never eat a chord that was meant for the app.
    #[test]
    fn a_modified_escape_is_not_the_bars() {
        let mut editor = TextFieldEditor::new("q");
        let modified = |platform, control, alt| {
            dispatch_search_key(
                &mut TextFieldEditor::new("q"),
                &mut FakeClipboard::default(),
                "escape",
                None,
                false,
                alt,
                platform,
                control,
                false,
            )
        };
        assert_eq!(modified(true, false, false), SearchKeyOutcome::Ignored);
        assert_eq!(modified(false, true, false), SearchKeyOutcome::Ignored);
        assert_eq!(modified(false, false, true), SearchKeyOutcome::Ignored);
        // …and the bare one still closes.
        assert_eq!(press(&mut editor, "escape", None), SearchKeyOutcome::Close);
    }

    /// The clipboard chords come through unchanged from the rename field — ⌘V
    /// pastes a query, ⌘C copies the selected part of it.
    #[test]
    fn the_clipboard_chords_still_work() {
        let mut clipboard = FakeClipboard {
            text: Some("needle".to_string()),
        };
        let mut editor = TextFieldEditor::new("");
        let outcome = dispatch_search_key(
            &mut editor,
            &mut clipboard,
            "v",
            Some("v"),
            false,
            false,
            true,
            false,
            false,
        );
        assert_eq!(outcome, SearchKeyOutcome::Edited);
        assert_eq!(editor.text(), "needle");
    }

    /// A ⌃⌘ chord propagates — in particular ⌃⌘c, which is how the user leaves
    /// copy mode (and so closes the bar) while the field holds focus. Without
    /// the ⌘+⌃ subtraction the rename field's ⌘C arm would read it as a copy and
    /// swallow it.
    #[test]
    fn a_ladder_chord_propagates() {
        let mut clipboard = FakeClipboard {
            text: Some("untouched".to_string()),
        };
        let mut editor = TextFieldEditor::with_selection("needle", 6);
        for key in ["c", "x", "v", "z", "/"] {
            let outcome = dispatch_search_key(
                &mut editor,
                &mut clipboard,
                key,
                Some(key),
                false,
                false,
                true,
                true,
                false,
            );
            assert_eq!(outcome, SearchKeyOutcome::Ignored, "⌃⌘{key} is the app's");
        }
        assert_eq!(editor.text(), "needle", "a chord must not edit the query");
        assert_eq!(
            clipboard.text.as_deref(),
            Some("untouched"),
            "⌃⌘C is not a copy"
        );
    }

    #[test]
    fn the_prompt_glyph_names_the_direction() {
        assert_eq!(search_prompt_glyph(true), "?");
        assert_eq!(search_prompt_glyph(false), "/");
    }

    #[test]
    fn a_bar_stales_when_focus_moves_or_copy_mode_ends() {
        // Live: its pane is focused and still in copy mode.
        assert!(!search_bar_is_stale("pane-a", Some("pane-a"), true));
        // The focused pane moved out from under it.
        assert!(search_bar_is_stale("pane-a", Some("pane-b"), true));
        // Its pane is gone entirely.
        assert!(search_bar_is_stale("pane-a", None, true));
        // Still focused, but the pane left copy mode (⌃⌘c fired while the bar
        // held focus, or Esc/q/y ended the mode from the grid).
        assert!(search_bar_is_stale("pane-a", Some("pane-a"), false));
    }
}
