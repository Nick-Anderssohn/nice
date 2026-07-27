//! Bug B regression gate — **execution model: mocked [`gpui::TestAppContext`],
//! ordinary libtest `#[gpui::test]` cases** (see `docs/testing.md` rule 4: it
//! needs a mounted view and gpui's real mouse/key dispatch, but never a pixel
//! and never a real OS event).
//!
//! ## What this pins
//!
//! The inline-rename field used to wire `on_mouse_down` and nothing else: the
//! press placed the caret, and holding the button while moving did nothing at
//! all. These cases drive the SHIPPED field through gpui's real dispatch path —
//! press, mouse-moves with the button held, release — and assert on the pure
//! editor model behind it:
//!
//!   * a press at boundary 2 then a move to boundary 6 selects `2..6`, and a
//!     further move PAST the field's right edge keeps extending (the reason the
//!     move/up listeners are registered at window level rather than on the
//!     field's own div, whose `on_mouse_move` only fires while hovered);
//!   * a release disarms the drag — a later held-button move with no new press
//!     extends nothing (the case that fails if the window `MouseUpEvent`
//!     listener is deleted; the existing release-then-button-less-move check
//!     cannot see that, because the move handler's own self-heal swallows a
//!     button-less move too);
//!   * a button-less move disarms the drag on its own, with no release involved
//!     at all (the self-heal that catches a press ending somewhere this field
//!     never saw), proven by a following held-button move that still extends
//!     nothing;
//!   * a move that rounds back to the boundary already dragged to fires no
//!     callback — the sub-glyph twitch dedupe — while the next move to a
//!     different boundary does;
//!   * typing after the drag replaces exactly the dragged range;
//!   * ending the edit mid-drag (commit / cancel / blur — here: the owner stops
//!     rendering the field) silently ends the tracking: later moves reach no
//!     callback and mutate nothing, no panic. This is the leak the plan's Risks
//!     section calls out, and it is asserted on the RAW callback count, not on
//!     an owner-side "am I still editing" guard, so a leaked window listener
//!     fails the case even though production owners also guard;
//!   * the drag arm an edge-ended edit leaves behind in the per-VIEW probe cell
//!     does not reach the NEXT edit: rename-begin resets the cell, so a field
//!     mounted while the old press is still held extends nothing;
//!   * a double-click does NOT arm a drag — a twitch after one must not collapse
//!     the word the double-click just selected (word-wise extension from a
//!     double-click is deliberately out of scope).
//!
//! ## Why the module is `#[path]`-included, not imported
//!
//! Same constraint as the `visual_rename_caret` binary next door: `rename_field`
//! lives in the `nice` BINARY crate, which a dev/test crate cannot depend on, and
//! a mirrored copy of the wiring would prove nothing about the shipped field.
//! So this binary compiles the real `crates/nice/src/inline_rename.rs` source
//! directly. Unlike that `harness = false` binary this one IS a libtest target,
//! so `cfg(test)` is on and the included module's own unit tests come along and
//! run here a second time (they are pure, fast, and identical to what
//! `cargo test -p nice inline_rename` runs).
//!
//! The mocked platform's `NoopTextSystem` gives every BMP char the same advance
//! (`0.6 × font_size`), so shaping is deterministic here — but nothing below
//! hardcodes that: every x is read back from the boundary table the field's own
//! paint published, exactly as the production hit-test does.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    div, point, prelude::*, px, rgb, Context, Entity, FocusHandle, KeyDownEvent, Modifiers,
    MouseButton, MouseDownEvent, Pixels, Point, Render, TestAppContext, VisualTestContext, Window,
};

use nice_model::file_browser::TextFieldEditor;

#[allow(dead_code)]
#[path = "../../nice/src/inline_rename.rs"]
mod inline_rename;

use inline_rename::{
    apply_rename_click, dispatch_rename_key, field_probe_cell, field_text, rename_field,
    reset_field_probe, FieldColors, FieldProbeCell, RenameKeyOutcome,
};

/// A name with words, separators and repeated glyphs — the same shape as the
/// plan's live drag leg fixture.
const TEXT: &str = "alpha beta_gamma.txt";
/// The basename preselection a production rename-begin seeds for [`TEXT`]
/// (everything before the extension) — the selection a leaked drag arm from a
/// previous edit would clobber.
const PRESELECT: usize = 16;
const TEXT_SIZE: f32 = 14.0;
/// The editing row's height and the field's inset inside it, so a press lands
/// comfortably inside the field box and `text_left` is nowhere near 0.
const ROW_H: f32 = 30.0;
const FIELD_INSET_X: f32 = 24.0;
/// An x far to the right of the field but still inside the mocked window
/// (`TestPlatform`'s display is 1920×1080 and the test window is maximized) —
/// where the pointer has left the field horizontally mid-drag.
const FAR_RIGHT_X: f32 = 1600.0;

/// The view under test: one rename field wired exactly the way the three
/// production call sites wire it (`place_rename_cursor` / `extend_rename_selection`
/// / `dispatch_rename_key`), plus two counters the assertions read.
struct RenameRoot {
    focus: FocusHandle,
    editor: TextFieldEditor,
    probe: FieldProbeCell,
    /// The owner's "is a rename in flight" state: flipping it false is this
    /// test's stand-in for commit / cancel / blur (all three stop rendering the
    /// field, which is all the field's teardown depends on).
    editing: bool,
    /// How many times the field asked the owner to extend the selection.
    /// Deliberately incremented in a callback that then extends UNCONDITIONALLY,
    /// so a stale window listener after the edit ends shows up as both a bumped
    /// count and a mutated model.
    drag_calls: Rc<Cell<usize>>,
}

impl RenameRoot {
    /// Begin a (new) rename exactly the way the three production owners do:
    /// reset the per-view probe cell, then seed a fresh editor with the name
    /// preselected. The reset is the whole point — the cell outlives every
    /// field it has measured, drag arm included.
    fn begin(&mut self, cx: &mut Context<Self>) {
        reset_field_probe(&self.probe);
        self.editor = TextFieldEditor::with_selection(TEXT, PRESELECT);
        self.editing = true;
        cx.notify();
    }
}

impl Render for RenameRoot {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let field = self.editing.then(|| {
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
            let drag_calls = self.drag_calls.clone();
            div()
                .w_full()
                .h(px(ROW_H))
                .flex()
                .flex_row()
                .items_center()
                .px(px(FIELD_INSET_X))
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
                        drag_calls.set(drag_calls.get() + 1);
                        let _ = weak_drag.update(app, |this, cx| {
                            this.editor.extend_to(index);
                            cx.notify();
                        });
                    },
                ))
                .into_any_element()
        });
        div().size_full().flex().flex_col().children(field)
    }
}

/// Mount a fresh field in a maximized mocked window, run to a first paint (which
/// publishes the boundary table and registers the field's listeners), and focus
/// the field so key dispatch reaches it.
fn mount(cx: &mut TestAppContext) -> (Entity<RenameRoot>, FieldProbeCell, &mut VisualTestContext) {
    let probe = field_probe_cell();
    let (root, vcx) = {
        let probe = probe.clone();
        cx.add_window_view(move |_window, c: &mut Context<RenameRoot>| RenameRoot {
            focus: c.focus_handle(),
            editor: TextFieldEditor::new(TEXT),
            probe,
            editing: true,
            drag_calls: Rc::new(Cell::new(0)),
        })
    };
    vcx.run_until_parked();
    let focus = root.read_with(vcx, |this, _| this.focus.clone());
    vcx.update(|window, app| window.focus(&focus, app));
    vcx.run_until_parked();
    (root, probe, vcx)
}

/// The window x of char boundary `index`, read back from the table the field's
/// own paint published — the same numbers the caret is drawn from.
fn x_of(probe: &FieldProbeCell, index: usize) -> f32 {
    let p = probe.borrow();
    p.text_left + p.boundaries[index]
}

/// A point on the field's text row at window x `x`.
fn at(x: f32) -> Point<Pixels> {
    point(px(x), px(ROW_H / 2.0))
}

/// Read the editor's selection.
fn selection(root: &Entity<RenameRoot>, vcx: &mut VisualTestContext) -> (usize, usize) {
    root.read_with(vcx, |this, _| this.editor.selection())
}

/// Read the editor's text.
fn text(root: &Entity<RenameRoot>, vcx: &mut VisualTestContext) -> String {
    root.read_with(vcx, |this, _| this.editor.text())
}

/// How many times the field has asked the owner to extend the selection.
fn drag_calls(root: &Entity<RenameRoot>, vcx: &mut VisualTestContext) -> usize {
    root.read_with(vcx, |this, _| this.drag_calls.get())
}

/// Press at boundary `from`, then move (button held) to boundary `to`.
fn press_and_drag(probe: &FieldProbeCell, vcx: &mut VisualTestContext, from: usize, to: usize) {
    vcx.simulate_mouse_down(at(x_of(probe, from)), MouseButton::Left, Modifiers::none());
    vcx.simulate_mouse_move(at(x_of(probe, to)), MouseButton::Left, Modifiers::none());
}

/// Press, move with the button held, release: the pointer's path selects the
/// range it crossed — including after it has left the field's right edge.
#[gpui::test]
fn press_then_move_extends_the_selection(cx: &mut TestAppContext) {
    let (root, probe, vcx) = mount(cx);

    // The press alone is still just a caret (the old behavior, unchanged).
    vcx.simulate_mouse_down(at(x_of(&probe, 2)), MouseButton::Left, Modifiers::none());
    assert_eq!(
        selection(&root, vcx),
        (2, 2),
        "the press places a collapsed caret at the pressed boundary"
    );

    vcx.simulate_mouse_move(at(x_of(&probe, 6)), MouseButton::Left, Modifiers::none());
    assert_eq!(
        selection(&root, vcx),
        (2, 6),
        "moving with the button held extends the selection to the moved-to boundary"
    );

    // Past the field's right edge: the window-level listener keeps extending.
    vcx.simulate_mouse_move(at(FAR_RIGHT_X), MouseButton::Left, Modifiers::none());
    assert_eq!(
        selection(&root, vcx),
        (2, TEXT.chars().count()),
        "a drag past the field's right edge extends to the end of the text"
    );

    // Back to the left of the anchor: the anchor holds, the caret crosses it.
    vcx.simulate_mouse_move(at(x_of(&probe, 0)), MouseButton::Left, Modifiers::none());
    assert_eq!(
        selection(&root, vcx),
        (0, 2),
        "dragging back past the anchor selects leftward from it"
    );

    // Release ends the tracking; a later button-less move changes nothing.
    vcx.simulate_mouse_up(at(x_of(&probe, 0)), MouseButton::Left, Modifiers::none());
    vcx.simulate_mouse_move(at(x_of(&probe, 9)), None, Modifiers::none());
    assert_eq!(
        selection(&root, vcx),
        (0, 2),
        "after the release, moving the mouse no longer changes the selection"
    );
}

/// The release is what disarms the drag — not the absence of a pressed button on
/// the next move.
///
/// `press_then_move_extends_the_selection` releases and then moves with NO button
/// held, which the move handler's own self-heal would swallow anyway; it cannot
/// tell a working mouse-up listener from a deleted one. Here the move after the
/// release reports the button still HELD (no new press), so the self-heal branch
/// is not on the path: only the window `MouseUpEvent` listener can stop this
/// move from extending.
#[gpui::test]
fn a_release_disarms_the_drag_even_when_the_next_move_reports_the_button_held(
    cx: &mut TestAppContext,
) {
    let (root, probe, vcx) = mount(cx);

    press_and_drag(&probe, vcx, 2, 6);
    assert_eq!(selection(&root, vcx), (2, 6));
    let calls_during_drag = drag_calls(&root, vcx);

    vcx.simulate_mouse_up(at(x_of(&probe, 6)), MouseButton::Left, Modifiers::none());
    vcx.simulate_mouse_move(at(x_of(&probe, 9)), MouseButton::Left, Modifiers::none());

    assert_eq!(
        drag_calls(&root, vcx),
        calls_during_drag,
        "a held-button move after the release never asked the owner to extend"
    );
    assert_eq!(
        selection(&root, vcx),
        (2, 6),
        "…and the released drag's selection stands"
    );
}

/// A move that arrives with no button held disarms the drag on the spot, with no
/// release anywhere in the story: the press ended somewhere this field never saw
/// (the edit closed mid-drag under another window, say). The second move — button
/// held again, at a boundary the first never touched — is what proves the arm was
/// *cleared* rather than the button-less move merely skipped.
#[gpui::test]
fn a_button_less_move_disarms_the_drag_without_any_release(cx: &mut TestAppContext) {
    let (root, probe, vcx) = mount(cx);

    vcx.simulate_mouse_down(at(x_of(&probe, 2)), MouseButton::Left, Modifiers::none());
    assert_eq!(selection(&root, vcx), (2, 2));

    vcx.simulate_mouse_move(at(x_of(&probe, 6)), None, Modifiers::none());
    assert_eq!(
        drag_calls(&root, vcx),
        0,
        "a move with no button held never extends"
    );

    vcx.simulate_mouse_move(at(x_of(&probe, 9)), MouseButton::Left, Modifiers::none());
    assert_eq!(
        drag_calls(&root, vcx),
        0,
        "and the arm is gone: a later held-button move extends nothing either"
    );
    assert_eq!(
        selection(&root, vcx),
        (2, 2),
        "…so the caret the press placed is untouched"
    );
}

/// Sub-glyph twitch dedupe: a held-button move that still rounds to the boundary
/// the drag is already at fires no callback at all, while the next move to a
/// different boundary does. Without the dedupe the owner would be re-notified on
/// every pixel of pointer noise.
#[gpui::test]
fn a_move_landing_on_the_same_boundary_does_not_fire(cx: &mut TestAppContext) {
    let (root, probe, vcx) = mount(cx);

    vcx.simulate_mouse_down(at(x_of(&probe, 2)), MouseButton::Left, Modifiers::none());

    // A quarter of a glyph past boundary 2 is still nearest to boundary 2, so
    // the hit-test returns the boundary the drag was armed at.
    let twitch = x_of(&probe, 2) + (x_of(&probe, 3) - x_of(&probe, 2)) * 0.25;
    vcx.simulate_mouse_move(at(twitch), MouseButton::Left, Modifiers::none());
    assert_eq!(
        drag_calls(&root, vcx),
        0,
        "a move that rounds back to the pressed boundary asked for nothing"
    );
    assert_eq!(selection(&root, vcx), (2, 2), "…and left the caret collapsed");

    vcx.simulate_mouse_move(at(x_of(&probe, 6)), MouseButton::Left, Modifiers::none());
    assert_eq!(
        drag_calls(&root, vcx),
        1,
        "a move that reaches a NEW boundary does fire, exactly once"
    );
    assert_eq!(selection(&root, vcx), (2, 6));
}

/// The dragged range is a real selection: the next printable key replaces it.
#[gpui::test]
fn typing_after_a_drag_replaces_the_dragged_range(cx: &mut TestAppContext) {
    let (root, probe, vcx) = mount(cx);

    press_and_drag(&probe, vcx, 2, 6);
    vcx.simulate_mouse_up(at(x_of(&probe, 6)), MouseButton::Left, Modifiers::none());
    assert_eq!(selection(&root, vcx), (2, 6));

    // `->x` is gpui's test syntax for "this keystroke produced the character x".
    vcx.simulate_keystrokes("x->x");
    assert_eq!(
        text(&root, vcx),
        // 2..6 is "pha " (the space included) — replaced by the one typed char.
        "alxbeta_gamma.txt",
        "the typed char replaced exactly the dragged range"
    );
    assert_eq!(
        selection(&root, vcx),
        (3, 3),
        "and left a collapsed caret after the inserted char"
    );
}

/// Ending the edit mid-drag tears the tracking down with the field: no further
/// callback, no mutation, no panic.
#[gpui::test]
fn ending_the_edit_mid_drag_stops_the_tracking(cx: &mut TestAppContext) {
    let (root, probe, vcx) = mount(cx);

    press_and_drag(&probe, vcx, 2, 4);
    assert_eq!(selection(&root, vcx), (2, 4));
    let calls_during_drag = drag_calls(&root, vcx);
    assert_eq!(calls_during_drag, 1, "the one move extended the selection once");

    // Commit / cancel / blur all come down to this: the owner stops rendering
    // the field. The mouse button is still down and never released.
    root.update(vcx, |this, cx| {
        this.editing = false;
        cx.notify();
    });
    vcx.run_until_parked();

    let far = at(FAR_RIGHT_X);
    vcx.simulate_mouse_move(far, MouseButton::Left, Modifiers::none());
    vcx.simulate_mouse_move(at(x_of(&probe, 0)), MouseButton::Left, Modifiers::none());
    vcx.simulate_mouse_up(far, MouseButton::Left, Modifiers::none());

    assert_eq!(
        drag_calls(&root, vcx),
        calls_during_drag,
        "no window listener survived the field it was registered by"
    );
    assert_eq!(
        selection(&root, vcx),
        (2, 4),
        "and the model the closed field was editing was not touched"
    );
    assert_eq!(text(&root, vcx), TEXT, "…nor was its text");
}

/// An edit that ends mid-drag leaves `FieldProbe::drag` armed — no release ever
/// reaches the dead field's listeners. The NEXT edit in the same view must not
/// inherit it: rename-begin resets the per-view cell, so the fresh field's first
/// held-button move extends nothing.
#[gpui::test]
fn a_stale_drag_arm_does_not_reach_the_next_edit(cx: &mut TestAppContext) {
    let (root, probe, vcx) = mount(cx);

    // Edit 1 ends mid-drag with the button STILL DOWN (Escape while dragging):
    // no mouse-up is ever delivered, so nothing disarms the probe on the way
    // out, and the listeners that would have self-healed die with the field.
    press_and_drag(&probe, vcx, 2, 4);
    assert_eq!(selection(&root, vcx), (2, 4));
    root.update(vcx, |this, cx| {
        this.editing = false;
        cx.notify();
    });
    vcx.run_until_parked();
    let calls_after_edit_1 = drag_calls(&root, vcx);

    // Edit 2 begins while that same press is still held — the slow-second-click
    // begin path, the one case the move handler's button-not-pressed self-heal
    // never gets to see.
    root.update(vcx, |this, cx| this.begin(cx));
    vcx.run_until_parked();
    assert_eq!(
        selection(&root, vcx),
        (0, PRESELECT),
        "the new edit starts on its basename preselection"
    );

    vcx.simulate_mouse_move(at(x_of(&probe, 9)), MouseButton::Left, Modifiers::none());
    assert_eq!(
        drag_calls(&root, vcx),
        calls_after_edit_1,
        "a held-button move over a freshly begun field extends nothing"
    );
    assert_eq!(
        selection(&root, vcx),
        (0, PRESELECT),
        "…and the preselection the begin seeded is intact"
    );
}

/// A double-click selects the word and does NOT arm a drag: the twitch that
/// follows one must not collapse that selection.
#[gpui::test]
fn a_double_click_does_not_arm_a_drag(cx: &mut TestAppContext) {
    let (root, probe, vcx) = mount(cx);

    vcx.simulate_event(MouseDownEvent {
        position: at(x_of(&probe, 8)),
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 2,
        first_mouse: false,
    });
    let word = selection(&root, vcx);
    assert_eq!(word, (6, 16), "the double-click selected \"beta_gamma\"");

    vcx.simulate_mouse_move(at(x_of(&probe, 2)), MouseButton::Left, Modifiers::none());
    assert_eq!(
        selection(&root, vcx),
        word,
        "moving after a double-click leaves the selected word alone"
    );
    assert_eq!(
        drag_calls(&root, vcx),
        0,
        "…and never asked the owner to extend"
    );
}
