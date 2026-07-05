//! The shared inline-rename field — the small char-by-char text editor that both
//! R10's sidebar `TabRow` and R11's toolbar pane pill mount.
//!
//! ## Why this module exists (plan H2 conformance)
//!
//! R11's plan says "pill rename **reuses R10's gate + field**." R10 shipped the
//! *gate* as a real reusable seam ([`nice_model::InlineRenameClickGate`]) but
//! hand-rolled the *field* inline inside `SidebarShellView` — there was no field
//! component to reuse. This module is the conformance pre-work: it lifts that
//! inline editor out into one place both callers share, so "reuse the field" is
//! literally true rather than a re-implementation.
//!
//! The pinned gpui exposes **no** `TextInput` widget, so the field is a plain
//! `String` edited char-by-char: Enter commits, Backspace pops, a bare printable
//! char appends. The *commit* semantics differ per caller
//! ([`nice_model::TabModel::rename_tab`] vs [`nice_model::TabModel::rename_pane`]
//! — the R8 model behavior, which the field must not reimplement), so the key
//! handler is injected rather than baked in; only the pure editing rule
//! ([`apply_rename_key`]) and the field chrome ([`rename_field`]) are shared.

use gpui::{
    div, px, App, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Rgba,
    SharedString, Styled, Window,
};

use nice_theme::chrome_geometry::INNER_CORNER_RADIUS;

/// The block caret glyph appended to the draft while editing (there is no real
/// text-cursor at the pin). `SidebarView`'s inline field / `WindowToolbarView`'s
/// `TextField` stand-in.
pub(crate) const RENAME_CARET: &str = "\u{258F}"; // ▏

/// What feeding one keystroke to an in-flight rename draft should do.
pub(crate) enum RenameKeyOutcome {
    /// Enter — the caller commits the draft (its own `rename_*` model call).
    Commit,
    /// The draft was mutated (a char appended, or a Backspace popped): the caller
    /// should `cx.notify()` and consume the key.
    Edited,
    /// A key this field does not handle (arrow keys, a ⌘/⌃ chord, …): the caller
    /// leaves it to propagate. **Escape is intentionally not handled here** — the
    /// owner's Esc key binding cancels the rename (the DO-NOT-PORT `NSEvent`
    /// monitor's replacement), and that must still fire.
    Ignored,
}

/// Apply one keystroke to the in-flight rename `draft`, char-by-char. Pure and
/// gpui-free so the exact editing rule is unit-tested without a window. Mirrors
/// `SidebarShellView::on_rename_key` and `InlinePanePill.commitEdit`'s key path:
/// Enter → [`RenameKeyOutcome::Commit`]; Backspace → pop the last char; a bare
/// printable char (no ⌘/⌃ held, `key_char` present) → append; anything else →
/// [`RenameKeyOutcome::Ignored`].
pub(crate) fn apply_rename_key(
    draft: &mut String,
    key: &str,
    key_char: Option<&str>,
    platform_mod: bool,
    control_mod: bool,
) -> RenameKeyOutcome {
    match key {
        "enter" => RenameKeyOutcome::Commit,
        "backspace" => {
            draft.pop();
            RenameKeyOutcome::Edited
        }
        _ => {
            if !platform_mod && !control_mod {
                if let Some(ch) = key_char {
                    draft.push_str(ch);
                    return RenameKeyOutcome::Edited;
                }
            }
            RenameKeyOutcome::Ignored
        }
    }
}

/// The inline-rename field element: a focused, bordered box showing `draft` + the
/// block caret, wired to `on_key`. The shared chrome both the sidebar row and the
/// toolbar pill render while editing — `flex_1` so it fills the title slot, with
/// the caller supplying its palette colors, text size, key context, and (because
/// the model commit differs) the key handler. The caller builds `on_key` with
/// `cx.listener(...)` and dispatches through [`apply_rename_key`] inside it.
pub(crate) fn rename_field(
    focus: &FocusHandle,
    draft: &str,
    key_context: &'static str,
    bg: Rgba,
    border: Rgba,
    text_color: Rgba,
    text_size: f32,
    on_key: impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .track_focus(focus)
        .key_context(key_context)
        .flex_1()
        .px(px(6.0))
        .py(px(2.0))
        .rounded(px(INNER_CORNER_RADIUS))
        .bg(bg)
        .border(px(1.0))
        .border_color(border)
        .text_size(px(text_size))
        .text_color(text_color)
        .child(SharedString::from(format!("{draft}{RENAME_CARET}")))
        .on_key_down(on_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_requests_a_commit_without_touching_the_draft() {
        let mut draft = "hello".to_string();
        assert!(matches!(
            apply_rename_key(&mut draft, "enter", None, false, false),
            RenameKeyOutcome::Commit
        ));
        assert_eq!(draft, "hello", "Enter must not mutate the draft");
    }

    #[test]
    fn backspace_pops_the_last_char() {
        let mut draft = "hi".to_string();
        assert!(matches!(
            apply_rename_key(&mut draft, "backspace", None, false, false),
            RenameKeyOutcome::Edited
        ));
        assert_eq!(draft, "h");
    }

    #[test]
    fn backspace_on_empty_draft_is_edited_but_stays_empty() {
        let mut draft = String::new();
        assert!(matches!(
            apply_rename_key(&mut draft, "backspace", None, false, false),
            RenameKeyOutcome::Edited
        ));
        assert_eq!(draft, "");
    }

    #[test]
    fn a_bare_printable_char_appends() {
        let mut draft = "ab".to_string();
        assert!(matches!(
            apply_rename_key(&mut draft, "c", Some("c"), false, false),
            RenameKeyOutcome::Edited
        ));
        assert_eq!(draft, "abc");
    }

    #[test]
    fn a_command_chord_is_ignored_and_never_types() {
        // ⌘A (select-all-ish) must not append 'a' into the title.
        let mut draft = "name".to_string();
        assert!(matches!(
            apply_rename_key(&mut draft, "a", Some("a"), true, false),
            RenameKeyOutcome::Ignored
        ));
        assert_eq!(draft, "name");
    }

    #[test]
    fn a_control_chord_is_ignored() {
        let mut draft = "name".to_string();
        assert!(matches!(
            apply_rename_key(&mut draft, "a", Some("a"), false, true),
            RenameKeyOutcome::Ignored
        ));
        assert_eq!(draft, "name");
    }

    #[test]
    fn escape_is_ignored_here_so_the_owner_binding_cancels() {
        // Escape carries no key_char, so it falls through to Ignored and the
        // draft is untouched — the shell/pill Esc action does the cancelling.
        let mut draft = "name".to_string();
        assert!(matches!(
            apply_rename_key(&mut draft, "escape", None, false, false),
            RenameKeyOutcome::Ignored
        ));
        assert_eq!(draft, "name");
    }

    #[test]
    fn a_non_char_navigation_key_is_ignored() {
        let mut draft = "name".to_string();
        assert!(matches!(
            apply_rename_key(&mut draft, "left", None, false, false),
            RenameKeyOutcome::Ignored
        ));
        assert_eq!(draft, "name");
    }
}
