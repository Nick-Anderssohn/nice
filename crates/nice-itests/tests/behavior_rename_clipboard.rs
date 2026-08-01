//! The clipboard-chord regression gate — **execution model: mocked
//! [`gpui::TestAppContext`], ordinary libtest `#[gpui::test]` cases** (see
//! `docs/testing.md` rule 4: it needs a mounted view, gpui's real key dispatch
//! and a real `App`, but never a pixel and never a real OS event).
//!
//! ## What this pins that the dispatch unit tests cannot
//!
//! `dispatch_rename_key`'s ⌘C/⌘X/⌘V *rule* is unit-tested against an in-memory
//! `RenameClipboard` fake (`cargo test -p nice inline_rename`). What a fake can
//! never show is the WIRING the bug actually was — everything between a
//! keystroke arriving at the focused field and a string landing on the
//! clipboard:
//!
//!   * the ⌘-chords reach the field's `on_key_down` at all. This is the reported
//!     bug: they used to fall out of the dispatch as `Ignored`, so ⌘C/⌘V in a
//!     rename did nothing. A fake-driven unit test calls the dispatch directly
//!     and so cannot see a chord that never gets there.
//!   * the production `impl RenameClipboard for App` reads and writes the SAME
//!     clipboard the rest of the app does — asserted by driving the chords from
//!     one side (a keystroke) and the clipboard from the other
//!     (`App::write_to_clipboard` / `read_from_clipboard` in the test driver).
//!   * the call sites' `&mut **cx` — the `Context<T>` → `&mut App` deref the
//!     three production owners pass — does not blow up inside the entity update
//!     that the key listener is running in (the plan's Risks section calls out
//!     exactly this borrow tangle; a bad one panics the moment the chord fires).
//!
//! The equivalent live leg — `NICE_SELFTEST=file-browser`'s (d-clip), real
//! ⌘A/⌘C/⌘V CGEvents through the real system pasteboard — remains the ground
//! truth for "a real OS key event reaches this process's field" (`docs/testing.md`
//! rule 2). It needs the Accessibility (TCC) grant, so it cannot be the ONLY
//! evidence for this fix: these cases carry the same wiring claim on any host,
//! in `cargo test`, with no grant and no window server.
//!
//! ## Why the module is `#[path]`-included, not imported
//!
//! Same constraint as `behavior_rename_drag` and `visual_rename_caret` next
//! door: `rename_field` / `dispatch_rename_key` live in the `nice` BINARY crate,
//! which a dev/test crate cannot depend on, and a mirrored copy of the wiring
//! would prove nothing about the shipped field. So this binary compiles the real
//! `crates/nice/src/inline_rename.rs` source directly.

use gpui::{
    div, prelude::*, px, rgb, ClipboardItem, Context, Entity, FocusHandle, KeyDownEvent, Render,
    TestAppContext, VisualTestContext, Window,
};

use nice_model::file_browser::TextFieldEditor;

#[allow(dead_code)]
#[path = "../../nice/src/inline_rename.rs"]
mod inline_rename;

use inline_rename::{
    apply_rename_click, dispatch_rename_key, field_probe_cell, field_text, rename_field,
    FieldColors, FieldProbeCell, RenameKeyOutcome,
};

/// The name under edit — the live (d-clip) leg's fixture, so the two gates read
/// as the same scenario at two layers.
const TEXT: &str = "clipme.txt";
const TEXT_SIZE: f32 = 14.0;
const ROW_H: f32 = 30.0;

/// A clipboard payload with two runs of control characters between its segments
/// and one ordinary space that must survive verbatim — the sanitizer, end to end
/// through a real chord.
const MULTILINE: &str = "pa\r\n\tste me";
const SANITIZED: &str = "pa ste me";

/// The view under test: one rename field wired exactly the way the three
/// production call sites wire it — in particular, the key listener hands
/// `dispatch_rename_key` the `App` its `Context` derefs to, which is the whole
/// production clipboard path.
struct RenameRoot {
    focus: FocusHandle,
    editor: TextFieldEditor,
    probe: FieldProbeCell,
}

impl Render for RenameRoot {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = FieldColors {
            bg: rgb(0x101820),
            border: rgb(0x30404f),
            text: rgb(0xe8eef5),
            caret: rgb(0xff0000),
            selection: rgb(0x00ff00),
        };
        let text = field_text(&self.editor);
        let weak_key = cx.weak_entity();
        let weak_click = cx.weak_entity();
        let weak_drag = cx.weak_entity();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .w_full()
                    .h(px(ROW_H))
                    .flex()
                    .flex_row()
                    .items_center()
                    .text_size(px(TEXT_SIZE))
                    .child(rename_field(
                        &self.focus,
                        &text,
                        "TestRename",
                        colors,
                        TEXT_SIZE,
                        self.probe.clone(),
                        move |e: &KeyDownEvent, window, app| {
                            let ks = &e.keystroke;
                            let _ = weak_key.update(app, |this, cx| {
                                let outcome = dispatch_rename_key(
                                    &mut this.editor,
                                    // THE production wiring under test: the
                                    // owner's `Context<T>` derefs to the `App`
                                    // the clipboard chords go through.
                                    &mut **cx,
                                    &ks.key,
                                    ks.key_char.as_deref(),
                                    ks.modifiers.shift,
                                    ks.modifiers.alt,
                                    ks.modifiers.platform,
                                    ks.modifiers.control,
                                    window.capslock().on,
                                );
                                if matches!(outcome, RenameKeyOutcome::Edited) {
                                    cx.notify();
                                }
                            });
                        },
                        move |index, click_count, _window, app| {
                            let _ = weak_click.update(app, |this, cx| {
                                apply_rename_click(&mut this.editor, index, click_count);
                                cx.notify();
                            });
                        },
                        move |index, _window, app| {
                            let _ = weak_drag.update(app, |this, cx| {
                                this.editor.extend_to(index);
                                cx.notify();
                            });
                        },
                    )),
            )
            .into_any_element()
    }
}

/// Mount a fresh field in a mocked window, paint once, and focus the field so
/// key dispatch reaches it.
fn mount(cx: &mut TestAppContext) -> (Entity<RenameRoot>, &mut VisualTestContext) {
    let (root, vcx) = cx.add_window_view(|_window, c: &mut Context<RenameRoot>| RenameRoot {
        focus: c.focus_handle(),
        editor: TextFieldEditor::new(TEXT),
        probe: field_probe_cell(),
    });
    vcx.run_until_parked();
    let focus = root.read_with(vcx, |this, _| this.focus.clone());
    vcx.update(|window, app| window.focus(&focus, app));
    vcx.run_until_parked();
    (root, vcx)
}

/// The editor's text.
fn text(root: &Entity<RenameRoot>, vcx: &mut VisualTestContext) -> String {
    root.read_with(vcx, |this, _| this.editor.text())
}

/// The editor's selection as char offsets.
fn selection(root: &Entity<RenameRoot>, vcx: &mut VisualTestContext) -> (usize, usize) {
    root.read_with(vcx, |this, _| this.editor.selection())
}

/// Read the app's clipboard from the DRIVER side — the same `App` API the rest
/// of the app (and the production `RenameClipboard` impl) uses.
fn clipboard(vcx: &mut VisualTestContext) -> Option<String> {
    vcx.update(|_window, app| app.read_from_clipboard().and_then(|item| item.text()))
}

/// Seed the app's clipboard from the driver side, as the plan's cheaper
/// validation route prescribes.
fn set_clipboard(vcx: &mut VisualTestContext, contents: &str) {
    vcx.update(|_window, app| {
        app.write_to_clipboard(ClipboardItem::new_string(contents.to_string()))
    });
}

/// ⌘A then ⌘C: the chords reach the focused field and the selection lands on the
/// app's real clipboard, with the field itself untouched.
#[gpui::test]
fn command_c_copies_the_field_text_to_the_app_clipboard(cx: &mut TestAppContext) {
    let (root, vcx) = mount(cx);
    set_clipboard(vcx, "something else entirely");

    vcx.simulate_keystrokes("cmd-a");
    assert_eq!(
        selection(&root, vcx),
        (0, TEXT.chars().count()),
        "a ⌘A keystroke reached the focused field and selected the whole name"
    );

    vcx.simulate_keystrokes("cmd-c");
    assert_eq!(
        clipboard(vcx).as_deref(),
        Some(TEXT),
        "⌘C wrote the selection to the clipboard the driver reads — the production \
         `RenameClipboard for App` impl and the driver are on the same pasteboard"
    );
    assert_eq!(text(&root, vcx), TEXT, "and a copy edited nothing");
    assert_eq!(selection(&root, vcx), (0, TEXT.chars().count()));
}

/// The counterfactual for the case above: a ⌘C with a COLLAPSED selection must
/// leave the clipboard exactly as it was (NSTextField never clobbers it with an
/// empty string), even though the chord is consumed.
#[gpui::test]
fn command_c_with_no_selection_leaves_the_app_clipboard_alone(cx: &mut TestAppContext) {
    let (root, vcx) = mount(cx);
    set_clipboard(vcx, "previously copied");

    // The mounted editor starts with a collapsed caret at the end of the name.
    assert_eq!(selection(&root, vcx), (TEXT.chars().count(), TEXT.chars().count()));
    vcx.simulate_keystrokes("cmd-c");

    assert_eq!(
        clipboard(vcx).as_deref(),
        Some("previously copied"),
        "a ⌘C with nothing selected wrote nothing at all"
    );
    assert_eq!(text(&root, vcx), TEXT);
}

/// ⌘X copies through the same path AND deletes the selection.
#[gpui::test]
fn command_x_cuts_the_selection_to_the_app_clipboard(cx: &mut TestAppContext) {
    let (root, vcx) = mount(cx);

    vcx.simulate_keystrokes("cmd-a cmd-x");

    assert_eq!(
        clipboard(vcx).as_deref(),
        Some(TEXT),
        "⌘X put the cut text on the clipboard"
    );
    assert_eq!(text(&root, vcx), "", "…and removed it from the field");
    assert_eq!(selection(&root, vcx), (0, 0));
}

/// ⌘V of a DRIVER-SEEDED clipboard replaces the selection, then inserts at the
/// caret the paste left behind — the plan's cheaper validation route, asserted
/// on the field text.
#[gpui::test]
fn command_v_pastes_a_driver_seeded_clipboard_into_the_field(cx: &mut TestAppContext) {
    let (root, vcx) = mount(cx);
    set_clipboard(vcx, "pasted");

    vcx.simulate_keystrokes("cmd-a cmd-v");
    assert_eq!(
        text(&root, vcx),
        "pasted",
        "⌘V replaced the whole selected name with the clipboard text"
    );
    assert_eq!(
        selection(&root, vcx),
        (6, 6),
        "…leaving a collapsed caret after the inserted text"
    );

    // A second ⌘V at that caret inserts rather than replacing, and an ordinary
    // typed char afterwards proves the field is in a plain editing state.
    vcx.simulate_keystrokes("cmd-v");
    assert_eq!(text(&root, vcx), "pastedpasted");
    vcx.simulate_keystrokes("x->x");
    assert_eq!(text(&root, vcx), "pastedpastedx");
}

/// A multi-line / tabbed clipboard pastes as the ONE line the field can hold,
/// ordinary spaces intact.
#[gpui::test]
fn a_multiline_clipboard_pastes_as_one_sanitized_line(cx: &mut TestAppContext) {
    let (root, vcx) = mount(cx);
    set_clipboard(vcx, MULTILINE);

    vcx.simulate_keystrokes("cmd-a cmd-v");

    assert_eq!(
        text(&root, vcx),
        SANITIZED,
        "the newline and tab flattened to single spaces on the way in"
    );
}

/// A NON-TEXT clipboard (a copied image) pastes nothing and deletes nothing —
/// the one production line the dispatch fake cannot stand in for:
/// `impl RenameClipboard for App::read_text`'s `.and_then(|item| item.text())`
/// is what turns a text-less `ClipboardItem` into "insert nothing", and no
/// other layer seeds a non-text item.
#[gpui::test]
fn a_non_text_clipboard_pastes_nothing_and_keeps_the_selection(cx: &mut TestAppContext) {
    let (root, vcx) = mount(cx);
    // A throwaway "PNG" — the mocked platform's clipboard stores the item
    // as-is, so the bytes never need to decode; what matters is that
    // `ClipboardItem::text()` is None for it.
    let image = gpui::Image::from_bytes(gpui::ImageFormat::Png, vec![0x89, b'P', b'N', b'G']);
    vcx.update(|_window, app| app.write_to_clipboard(ClipboardItem::new_image(&image)));

    vcx.simulate_keystrokes("cmd-a cmd-v");

    assert_eq!(
        text(&root, vcx),
        TEXT,
        "⌘V of a text-less clipboard inserted nothing"
    );
    assert_eq!(
        selection(&root, vcx),
        (0, TEXT.chars().count()),
        "…and did NOT delete the selection the user was holding"
    );
}

/// The chord-guard counterfactual: ⌃V is not the paste chord (the arms guard on
/// `platform_mod`), so it must leave the field alone and keep propagating.
#[gpui::test]
fn a_control_v_chord_does_not_paste(cx: &mut TestAppContext) {
    let (root, vcx) = mount(cx);
    set_clipboard(vcx, "nope");

    vcx.simulate_keystrokes("cmd-a ctrl-v");

    assert_eq!(text(&root, vcx), TEXT, "⌃V pasted nothing");
    assert_eq!(
        selection(&root, vcx),
        (0, TEXT.chars().count()),
        "…and did not touch the selection either"
    );
}
