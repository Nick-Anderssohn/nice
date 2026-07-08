//! The Settings ▸ Shortcuts pane — R24's recorder field board (What-to-build item
//! 5, §Recorder). Fills R23's `shortcuts_pane` seam ([`crate::settings::root::shortcuts_pane`]
//! delegates here); it reuses R23's `SettingRow` / `SettingTitle` blocks and reads
//! the live [`ShortcutBindings`] store.
//!
//! ## Layout
//! One [`setting_row`](crate::settings::root::setting_row) per rebindable
//! [`ShortcutAction`] (all 13, `ShortcutAction::ALL` order). Each row's control is a
//! **recorder field**:
//! * **Resting** — the bound combo rendered as key-pills (⌘⌥ symbols + the key), or
//!   `"Not bound"` when the action is unbound; clicking it enters capture mode. A
//!   per-action **Reset** button appears iff `!is_at_default` (Swift
//!   `KeyRecorderField.swift:62-65, 277-279`).
//! * **Capture** — a focus-scoped div (`key_context "ShortcutRecorder"`) whose
//!   `on_key_down` reads the chord. Plain Escape cancels; a conflicting combo shows
//!   an `"Already used by <label>"` row with **Replace** / **Cancel**; a free combo
//!   commits and tears down (§Recorder).
//!
//! ## Capture mechanism (D3, adapted to the gpui pin)
//! D3 specifies "a focus-scoped `on_key_down` that stops propagation so the chord
//! never reaches a keymap binding". At this gpui pin the dispatch order is **action
//! bindings BEFORE key-down listeners** (`window.rs:4872-4888`: a bound chord fires
//! its action and stops propagation, so `on_key_down` never sees it) — so a bare
//! `on_key_down` cannot preempt a chord that is already bound, which is exactly the
//! case conflict detection must capture (⌘T, ⌘B, …). The faithful realization of
//! D3's rationale ("read the chord without triggering it") is therefore to **stand
//! the keymap down while recording** — [`enter_record`] calls
//! [`App::clear_key_bindings`], so every chord falls through to the recorder's
//! `on_key_down`; [`teardown`] calls [`crate::keymap::rebuild_keymap`] to restore
//! the full board (incl. any freshly-committed binding). This is a *closer* port of
//! Swift than the plan's wording: Swift stands its `KeyboardShortcutMonitor` down
//! and installs a higher-priority local monitor while recording
//! (`KeyRecorderField.swift:152-170`). The keymap outage is bounded to the capture
//! interaction and torn down on commit / cancel / Esc / **focus-out** (rail switch,
//! window close, click-away — the `.onDisappear` guarantee, via
//! [`Window::on_focus_out`]).
//!
//! ## Recorder state
//! [`RecorderState`] is a gpui `Global` holding the transient capture state (which
//! action is recording, a pending combo + its conflict) plus the recorder's
//! [`FocusHandle`] — the same stateless-pane-over-a-Global shape R23's
//! `ImportFeedback` uses (the pane stays a pure builder; every interaction runs on
//! `&mut App` and mutates the Global). It is installed lazily on the first
//! [`enter_record`] (no path, no disk — hermetic), so a never-touched pane never
//! allocates it.

// A few reader/driver fns are consumed only by the `settings-window` scenario
// (always-compiled) + the shipped pane; the deliberately-exported-surface pattern.
#![allow(dead_code)]

use gpui::{
    div, prelude::*, px, AnyElement, App, FocusHandle, Global, KeyDownEvent, MouseButton, Rgba,
    SharedString, Subscription, Window,
};

use nice_model::shortcuts::{conflicting_action, Modifiers, OwnedCombo, ShortcutAction};

use crate::settings::root::{setting_row, setting_title};
use crate::shortcuts_store::ShortcutBindings;
use crate::theme::slot_to_rgba;

// ===========================================================================
// Pure capture decision (gpui-free — #[test]-covered)
// ===========================================================================

/// What a single captured keystroke means while recording `recording` (§Recorder
/// capture, `KeyRecorderField.swift:204-228`). Pure — the on_key_down handler maps
/// a gpui `Keystroke` into `(modifiers, key, is_held)` and calls [`decide_capture`],
/// then acts on the outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CaptureOutcome {
    /// An auto-repeat (`KeyDownEvent.is_held`) — ignored, stay recording.
    Ignore,
    /// Plain Escape (no modifiers) — cancel the capture, no change.
    Cancel,
    /// A free combo — commit it to `recording` and tear down.
    Commit(OwnedCombo),
    /// A combo already held by `other` — stay recording, show the conflict row so
    /// the user resolves via Replace / Cancel (no silent overwrite).
    Conflict {
        combo: OwnedCombo,
        other: ShortcutAction,
    },
}

/// Decide what a captured keystroke means (pure, §Recorder). `bindings` is the live
/// `(action, Option<combo>)` map (for the intra-table conflict check). Auto-repeat
/// is ignored; plain Escape cancels; Escape WITH modifiers is a legit combo; a combo
/// held by another rebindable action conflicts; else it commits.
pub(crate) fn decide_capture(
    recording: ShortcutAction,
    modifiers: Modifiers,
    key: &str,
    is_held: bool,
    bindings: &[(ShortcutAction, Option<OwnedCombo>)],
) -> CaptureOutcome {
    // Ignore auto-repeat (`is_held`) — a held key must not spam captures.
    if is_held {
        return CaptureOutcome::Ignore;
    }
    // Plain Escape (no modifiers) cancels; Escape + modifiers is a real combo.
    if key == "escape" && modifiers == Modifiers::default() {
        return CaptureOutcome::Cancel;
    }
    let combo = OwnedCombo {
        modifiers,
        key: key.to_string(),
    };
    let view = bindings.iter().map(|(a, c)| (*a, c.as_ref()));
    match conflicting_action(view, &combo, recording) {
        Some(other) => CaptureOutcome::Conflict { combo, other },
        None => CaptureOutcome::Commit(combo),
    }
}

/// The conflict-row copy (§Recorder conflict row, `KeyRecorderField.swift:113-123`).
pub(crate) fn conflict_message(other: ShortcutAction) -> String {
    format!("Already used by {}", other.label())
}

// ===========================================================================
// Recorder state (the gpui Global)
// ===========================================================================

/// The transient recorder capture state + the recorder's focus handle. A gpui
/// `Global` (the stateless-pane-over-a-Global shape, mirroring R23's
/// `ImportFeedback`). Installed lazily by [`enter_record`].
struct RecorderState {
    /// The recorder's focus handle — focused while capturing so the chord routes to
    /// its `on_key_down`; the focus-out subscription tears the capture down.
    focus: FocusHandle,
    /// The action currently in capture mode, or `None` at rest.
    recording: Option<ShortcutAction>,
    /// A captured combo pending conflict resolution (set only while conflicting).
    pending: Option<OwnedCombo>,
    /// The action `pending` collides with (the Replace target).
    conflict: Option<ShortcutAction>,
    /// Live focus-out subscription: leaving the recorder (rail switch, window close,
    /// click-away) tears the capture down and restores the keymap.
    blur_sub: Option<Subscription>,
}

impl Global for RecorderState {}

/// The action currently recording, or `None`. Absent Global ⇒ `None` (never touched).
pub(crate) fn recording_action(cx: &App) -> Option<ShortcutAction> {
    cx.try_global::<RecorderState>().and_then(|r| r.recording)
}

/// The combo pending conflict resolution, or `None`.
pub(crate) fn pending_combo(cx: &App) -> Option<OwnedCombo> {
    cx.try_global::<RecorderState>()
        .and_then(|r| r.pending.clone())
}

/// The action the pending combo conflicts with (the Replace target), or `None`.
pub(crate) fn conflict_action(cx: &App) -> Option<ShortcutAction> {
    cx.try_global::<RecorderState>().and_then(|r| r.conflict)
}

fn recorder_focus(cx: &App) -> Option<FocusHandle> {
    cx.try_global::<RecorderState>().map(|r| r.focus.clone())
}

/// Enter capture mode for `action`: stand the keymap down (D3, so the chord reaches
/// the recorder's `on_key_down` rather than firing an action), focus the recorder,
/// and arm the focus-out teardown. Idempotent-ish — re-entering on another action
/// resets the capture state to that action.
pub(crate) fn enter_record(window: &mut Window, cx: &mut App, action: ShortcutAction) {
    // Install the Global (with a fresh focus handle) on first use — no path, no
    // disk, so it never touches persisted state (hermetic).
    if cx.try_global::<RecorderState>().is_none() {
        let focus = cx.focus_handle();
        cx.set_global(RecorderState {
            focus,
            recording: None,
            pending: None,
            conflict: None,
            blur_sub: None,
        });
    }

    // Swift monitor stand-down parity (D3): clear the live keymap so the captured
    // chord dispatches NO action; `teardown` rebuilds it.
    cx.clear_key_bindings();

    let focus = cx.global::<RecorderState>().focus.clone();
    focus.focus(window, cx);
    // Focus-out ⇒ teardown (rail switch / window close / click-away) — restores the
    // keymap so a mid-record disappear can never strand the capture or the outage.
    let sub = window.on_focus_out(&focus, cx, |_e, _window, cx| {
        if recording_action(cx).is_some() {
            teardown(cx);
        }
    });

    let state = cx.global_mut::<RecorderState>();
    state.recording = Some(action);
    state.pending = None;
    state.conflict = None;
    state.blur_sub = Some(sub);
    cx.refresh_windows();
}

/// Apply a captured key-down while recording: decide the outcome and act (§Recorder).
fn apply_key_down(cx: &mut App, event: &KeyDownEvent) {
    let Some(action) = recording_action(cx) else {
        return;
    };
    let ks = &event.keystroke;
    let modifiers = to_model_modifiers(&ks.modifiers);
    let bindings = current_bindings(cx);
    match decide_capture(action, modifiers, &ks.key, event.is_held, &bindings) {
        CaptureOutcome::Ignore => {}
        CaptureOutcome::Cancel => teardown(cx),
        CaptureOutcome::Commit(combo) => {
            // Persist + rebuild (set_binding), then clear the capture state. The
            // subsequent teardown rebuild is a harmless no-op-equivalent.
            ShortcutBindings::set_binding(cx, action, Some(combo));
            teardown(cx);
        }
        CaptureOutcome::Conflict { combo, other } => {
            if cx.try_global::<RecorderState>().is_some() {
                let state = cx.global_mut::<RecorderState>();
                state.pending = Some(combo);
                state.conflict = Some(other);
            }
            cx.refresh_windows();
        }
    }
    // The recorder consumed the chord — no other key listener should act on it (the
    // keymap is already down, so no action would fire; this guards sibling listeners).
    cx.stop_propagation();
}

/// Resolve a conflict with **Replace** (§Recorder conflict row): clear the losing
/// action's binding AND set this action's pending combo, then tear down. A no-op
/// (just teardown) if the capture state is incomplete.
pub(crate) fn resolve_replace(cx: &mut App) {
    let (Some(action), Some(combo), Some(loser)) =
        (recording_action(cx), pending_combo(cx), conflict_action(cx))
    else {
        teardown(cx);
        return;
    };
    // Clear the loser first (so the combo is free), then bind this action.
    ShortcutBindings::set_binding(cx, loser, None);
    ShortcutBindings::set_binding(cx, action, Some(combo));
    teardown(cx);
}

/// Cancel the capture (§Recorder Cancel / plain Esc): tear down, no change.
pub(crate) fn cancel_capture(cx: &mut App) {
    teardown(cx);
}

/// Restore `action` to its default combo (§Reset-to-defaults): `ShortcutBindings::reset`
/// (persist + rebuild). Swift's per-action Reset (`isAtDefault` drives the button).
pub(crate) fn reset_action(cx: &mut App, action: ShortcutAction) {
    ShortcutBindings::reset(cx, action);
}

/// Tear the capture down: reset the transient state AND restore the keymap
/// [`enter_record`] stood down. Called on commit / cancel / Esc / focus-out; leaves
/// the focus-out subscription in place (replaced on the next [`enter_record`]).
fn teardown(cx: &mut App) {
    if cx.try_global::<RecorderState>().is_some() {
        let state = cx.global_mut::<RecorderState>();
        state.recording = None;
        state.pending = None;
        state.conflict = None;
    }
    // Restore the full keymap (the 13 live combos + the non-rebindable set), picking
    // up any binding just committed. The SOLE rebind owner (D2).
    crate::keymap::rebuild_keymap(cx);
    cx.refresh_windows();
}

/// The live `(action, Option<combo>)` map (for the conflict check). Empty when no
/// store Global is installed (no conflicts).
fn current_bindings(cx: &App) -> Vec<(ShortcutAction, Option<OwnedCombo>)> {
    match cx.try_global::<ShortcutBindings>() {
        Some(store) => ShortcutAction::ALL
            .into_iter()
            .map(|a| (a, store.binding(a)))
            .collect(),
        None => Vec::new(),
    }
}

/// gpui `Modifiers` → the gpui-free model `Modifiers` (⌘ = the platform key on macOS).
fn to_model_modifiers(m: &gpui::Modifiers) -> Modifiers {
    Modifiers {
        command: m.platform,
        control: m.control,
        alt: m.alt,
        shift: m.shift,
    }
}

// ===========================================================================
// Rendering
// ===========================================================================

/// The row colours, read once from the active chrome slots and threaded into the
/// control builders (mirrors `appearance_pane`'s `Rgba` threading).
#[derive(Clone, Copy)]
struct RowColors {
    ink: Rgba,
    ink2: Rgba,
    ink3: Rgba,
    line: Rgba,
    /// The pill fill (a faint ink wash) + capture-mode background.
    fill: Rgba,
    /// The conflict-row warning tint (a warm red, matching the modal's destructive fill).
    warn: Rgba,
}

/// The Shortcuts pane body (R23 seam). One recorder row per rebindable action.
pub(crate) fn shortcuts_pane(_window: &mut Window, cx: &mut App) -> AnyElement {
    let slots = crate::theme_settings::active_chrome_slots(cx);
    let colors = RowColors {
        ink: slot_to_rgba(slots.ink),
        ink2: slot_to_rgba(slots.ink2),
        ink3: slot_to_rgba(slots.ink3),
        line: slot_to_rgba(slots.line),
        fill: slot_to_rgba(slots.background2),
        warn: gpui::rgb(0xC0_39_2B),
    };

    let recording = recording_action(cx);
    let pending = pending_combo(cx);
    let conflict = conflict_action(cx);

    let mut col = div()
        .flex()
        .flex_col()
        .child(setting_title("Shortcuts", cx))
        .child(
            div()
                .pb(px(8.0))
                .text_size(px(12.0))
                .text_color(colors.ink2)
                .child("Click a shortcut to record a new key combination. Press Esc to cancel."),
        );

    for action in ShortcutAction::ALL {
        let control: AnyElement = if recording == Some(action) {
            let focus = recorder_focus(cx).expect("recording implies the RecorderState is installed");
            capture_control(focus, pending.clone(), conflict, colors)
        } else {
            let combo = cx.try_global::<ShortcutBindings>().and_then(|s| s.binding(action));
            let at_default = cx
                .try_global::<ShortcutBindings>()
                .map(|s| s.is_at_default(action))
                .unwrap_or(true);
            resting_control(action, combo, at_default, colors)
        };
        col = col.child(setting_row(
            SharedString::from(action.label()),
            None,
            control,
            cx,
        ));
    }

    col.into_any_element()
}

/// The resting control: the bound combo as key-pills (or "Not bound"), clickable to
/// enter recording, plus a Reset button iff `!is_at_default`.
fn resting_control(
    action: ShortcutAction,
    combo: Option<OwnedCombo>,
    is_at_default: bool,
    colors: RowColors,
) -> AnyElement {
    let field_id = SharedString::from(format!("settings.shortcuts.{}.field", action.id()));
    let mut field = div()
        .id(field_id)
        .role(gpui::Role::Button)
        .aria_label(SharedString::from(action.label()))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .min_w(px(120.0))
        .justify_end()
        .cursor_pointer();
    match &combo {
        Some(combo) => {
            for label in combo_pill_labels(combo) {
                field = field.child(key_pill(label, colors));
            }
        }
        None => {
            field = field.child(
                div()
                    .text_size(px(12.0))
                    .text_color(colors.ink3)
                    .child("Not bound"),
            );
        }
    }
    let field = field.on_mouse_down(
        MouseButton::Left,
        move |_e, window, cx: &mut App| enter_record(window, cx, action),
    );

    let mut row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .child(field);
    if !is_at_default {
        row = row.child(reset_button(action, colors));
    }
    row.into_any_element()
}

/// The capture control: a focus-scoped `on_key_down` div showing the recording
/// prompt (+ pending pills once a combo is held), and the conflict row when the
/// captured combo collides.
fn capture_control(
    focus: FocusHandle,
    pending: Option<OwnedCombo>,
    conflict: Option<ShortcutAction>,
    colors: RowColors,
) -> AnyElement {
    // The recording capsule (§Recorder recording capsule).
    let mut capsule = div()
        .id("settings.shortcuts.capture")
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .px(px(8.0))
        .py(px(4.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(colors.line)
        .bg(colors.fill);
    match &pending {
        Some(combo) => {
            for label in combo_pill_labels(combo) {
                capsule = capsule.child(key_pill(label, colors));
            }
            capsule = capsule.child(
                div()
                    .text_size(px(11.5))
                    .text_color(colors.ink2)
                    .child("Press another combo, or resolve below"),
            );
        }
        None => {
            capsule = capsule.child(
                div()
                    .text_size(px(12.0))
                    .text_color(colors.ink2)
                    .child("Press a combo… (Esc to cancel)"),
            );
        }
    }

    let mut inner = div().flex().flex_col().gap(px(6.0)).child(capsule);
    if let Some(other) = conflict {
        inner = inner.child(conflict_row(other, colors));
    }

    div()
        .track_focus(&focus)
        .key_context("ShortcutRecorder")
        .on_key_down(move |event, _window, cx| apply_key_down(cx, event))
        .child(inner)
        .into_any_element()
}

/// The conflict row (§Recorder conflict row): "Already used by <label>" + Replace +
/// Cancel.
fn conflict_row(other: ShortcutAction, colors: RowColors) -> impl IntoElement {
    let replace = div()
        .id("settings.shortcuts.replace")
        .role(gpui::Role::Button)
        .aria_label("Replace")
        .px(px(10.0))
        .py(px(3.0))
        .rounded(px(6.0))
        .bg(colors.warn)
        .text_size(px(12.0))
        .text_color(gpui::white())
        .cursor_pointer()
        .child("Replace")
        .on_mouse_down(MouseButton::Left, move |_e, _window, cx: &mut App| {
            resolve_replace(cx);
        });
    let cancel = div()
        .id("settings.shortcuts.cancel")
        .role(gpui::Role::Button)
        .aria_label("Cancel")
        .px(px(10.0))
        .py(px(3.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(colors.line)
        .text_size(px(12.0))
        .text_color(colors.ink)
        .cursor_pointer()
        .child("Cancel")
        .on_mouse_down(MouseButton::Left, move |_e, _window, cx: &mut App| {
            cancel_capture(cx);
        });
    div()
        .id("settings.shortcuts.conflict")
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(
            div()
                .text_size(px(11.5))
                .text_color(colors.warn)
                .child(SharedString::from(conflict_message(other))),
        )
        .child(div().flex().flex_row().gap(px(6.0)).child(replace).child(cancel))
}

/// A per-action Reset button (a11y `settings.shortcuts.<id>.reset`) — shown only
/// when the action is off its default.
fn reset_button(action: ShortcutAction, colors: RowColors) -> impl IntoElement {
    div()
        .id(SharedString::from(format!(
            "settings.shortcuts.{}.reset",
            action.id()
        )))
        .role(gpui::Role::Button)
        .aria_label("Reset")
        .px(px(8.0))
        .py(px(3.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(colors.line)
        .text_size(px(11.5))
        .text_color(colors.ink2)
        .cursor_pointer()
        .child("Reset")
        .on_mouse_down(MouseButton::Left, move |_e, _window, cx: &mut App| {
            reset_action(cx, action);
        })
}

/// One key-pill (a labeled rounded chip) — the resting/pending combo rendering.
fn key_pill(label: String, colors: RowColors) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_center()
        .min_w(px(20.0))
        .px(px(6.0))
        .py(px(2.0))
        .rounded(px(5.0))
        .bg(colors.fill)
        .border_1()
        .border_color(colors.line)
        .text_size(px(12.0))
        .text_color(colors.ink)
        .child(SharedString::from(label))
}

/// The pill labels for a combo — the modifier symbols (⌘⌃⌥⇧, canonical order) then
/// the key glyph.
fn combo_pill_labels(combo: &OwnedCombo) -> Vec<String> {
    let mut v = Vec::new();
    if combo.modifiers.command {
        v.push("⌘".to_string());
    }
    if combo.modifiers.control {
        v.push("⌃".to_string());
    }
    if combo.modifiers.alt {
        v.push("⌥".to_string());
    }
    if combo.modifiers.shift {
        v.push("⇧".to_string());
    }
    v.push(key_display(&combo.key));
    v
}

/// A key token as a user-facing glyph (arrows → ↑↓←→, letters uppercased, else the
/// token as-is). Faithful-not-identical, like the appearance pickers.
fn key_display(key: &str) -> String {
    match key {
        "up" => "↑".to_string(),
        "down" => "↓".to_string(),
        "left" => "←".to_string(),
        "right" => "→".to_string(),
        _ => key.to_uppercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default map as an owned `(action, Some(combo))` list — the conflict view.
    fn default_bindings_vec() -> Vec<(ShortcutAction, Option<OwnedCombo>)> {
        nice_model::shortcuts::default_bindings()
            .into_iter()
            .map(|(a, c)| (a, Some(OwnedCombo::from(c))))
            .collect()
    }

    fn combo(token: &str) -> OwnedCombo {
        OwnedCombo::from_token(token).unwrap()
    }

    /// Auto-repeat (`is_held`) is ignored — a held key does not spam captures
    /// (§Recorder capture, `KeyRecorderField.swift:204-228`).
    #[test]
    fn auto_repeat_is_ignored() {
        let out = decide_capture(
            ShortcutAction::NewTerminalPane,
            Modifiers::COMMAND,
            "y",
            true, // is_held
            &default_bindings_vec(),
        );
        assert_eq!(out, CaptureOutcome::Ignore);
    }

    /// Plain Escape (no modifiers) cancels (`KeyRecorderField.swift:214-217`).
    #[test]
    fn plain_escape_cancels() {
        let out = decide_capture(
            ShortcutAction::NewTerminalPane,
            Modifiers::default(),
            "escape",
            false,
            &default_bindings_vec(),
        );
        assert_eq!(out, CaptureOutcome::Cancel);
    }

    /// Escape WITH modifiers is a legit combo — it commits, it does not cancel
    /// (`KeyRecorderField.swift:214-217`). ⌘⇧-escape is free in the default table.
    #[test]
    fn escape_with_modifiers_commits() {
        let out = decide_capture(
            ShortcutAction::NewTerminalPane,
            Modifiers::COMMAND_SHIFT,
            "escape",
            false,
            &default_bindings_vec(),
        );
        assert_eq!(out, CaptureOutcome::Commit(combo("cmd-shift-escape")));
    }

    /// A free combo commits to the recording action.
    #[test]
    fn free_combo_commits() {
        let out = decide_capture(
            ShortcutAction::NewTerminalPane,
            Modifiers::COMMAND,
            "y",
            false,
            &default_bindings_vec(),
        );
        assert_eq!(out, CaptureOutcome::Commit(combo("cmd-y")));
    }

    /// A combo held by another action conflicts — stay recording, surface the loser
    /// (`KeyRecorderField.swift:113-123`; the intra-table rule
    /// `KeyboardShortcuts.swift:238-252`). ⌘B is ToggleSidebar's default.
    #[test]
    fn conflicting_combo_yields_pending_and_conflict() {
        let out = decide_capture(
            ShortcutAction::NewTerminalPane,
            Modifiers::COMMAND,
            "b",
            false,
            &default_bindings_vec(),
        );
        assert_eq!(
            out,
            CaptureOutcome::Conflict {
                combo: combo("cmd-b"),
                other: ShortcutAction::ToggleSidebar,
            }
        );
    }

    /// Re-capturing the action's OWN current combo is not a self-conflict (the
    /// `excluding` rule): ⌘T on NewTerminalPane commits.
    #[test]
    fn recapturing_own_combo_is_not_a_self_conflict() {
        let out = decide_capture(
            ShortcutAction::NewTerminalPane,
            Modifiers::COMMAND,
            "t",
            false,
            &default_bindings_vec(),
        );
        assert_eq!(out, CaptureOutcome::Commit(combo("cmd-t")));
    }

    /// The conflict-row copy is "Already used by <label>".
    #[test]
    fn conflict_message_names_the_other_label() {
        assert_eq!(
            conflict_message(ShortcutAction::ToggleSidebar),
            "Already used by Toggle sidebar"
        );
    }

    /// §Reset-to-defaults: the recorder's `reset_action` (→ `ShortcutBindings::reset`)
    /// restores the default combo AND `is_at_default` flips back on (Swift
    /// `KeyRecorderField.swift:277-279` — `isAtDefault` drives the Reset button).
    /// Drives the App-level mutator (slice 1 covered only the in-memory helper).
    #[gpui::test]
    fn reset_action_restores_default_and_flips_is_at_default(cx: &mut gpui::TestAppContext) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nice-recorder-reset-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        cx.update(|app| {
            app.set_global(ShortcutBindings::with_defaults(dir.join("ui_settings.json")));
            // Off-default: rebind newTerminalPane ⌘T -> ⌘Y (persist + rebuild inside).
            ShortcutBindings::set_binding(
                app,
                ShortcutAction::NewTerminalPane,
                OwnedCombo::from_token("cmd-y"),
            );
            assert!(!app
                .global::<ShortcutBindings>()
                .is_at_default(ShortcutAction::NewTerminalPane));

            // Reset through the recorder's per-action Reset path.
            reset_action(app, ShortcutAction::NewTerminalPane);

            let store = app.global::<ShortcutBindings>();
            assert_eq!(
                store.binding(ShortcutAction::NewTerminalPane),
                OwnedCombo::from_token("cmd-t"),
                "Reset restored the default combo"
            );
            assert!(
                store.is_at_default(ShortcutAction::NewTerminalPane),
                "is_at_default flips back on after Reset (the button hides)"
            );
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The pill labels are the modifier glyphs (canonical order) then the key glyph;
    /// arrows and letters render as glyphs.
    #[test]
    fn combo_pill_labels_render_glyphs() {
        assert_eq!(
            combo_pill_labels(&combo("cmd-alt-down")),
            vec!["⌘", "⌥", "↓"]
        );
        assert_eq!(combo_pill_labels(&combo("cmd-shift-b")), vec!["⌘", "⇧", "B"]);
        assert_eq!(combo_pill_labels(&combo("cmd--")), vec!["⌘", "-"]);
    }

    // ===================================================================
    // App-level recorder state machine (#[gpui::test], TestAppContext)
    // ===================================================================

    /// A unique `ui_settings.json` temp path for a store the gpui tests install.
    /// Never the real support root (hermeticity); the store's persist is fail-soft,
    /// but a real unique path keeps the write a collision-free no-op.
    fn unique_temp_ui_settings(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nice-recorder-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("ui_settings.json")
    }

    /// A synthetic `KeyDownEvent` for `chord` (e.g. `"cmd-b"`), as the recorder's
    /// `on_key_down` would receive it — the App-side driver for [`apply_key_down`].
    fn key_down(chord: &str) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: gpui::Keystroke::parse(chord).expect("test chord parses"),
            is_held: false,
            prefer_character_input: false,
        }
    }

    /// The recorder state machine's Conflict and Commit arms (`apply_key_down`),
    /// plus the plain-Esc Cancel arm — the App-side wiring that acts on
    /// [`decide_capture`]'s outcome. Previously reachable only from the
    /// accessibility-gated `settings-window` scenario; this drives it directly on a
    /// `TestAppContext` (the `reset_action` style). A bare window gives `enter_record`
    /// something to focus into; the outcomes are asserted on `&mut App`.
    #[gpui::test]
    fn apply_key_down_conflict_then_commit_then_cancel(cx: &mut gpui::TestAppContext) {
        let path = unique_temp_ui_settings("apply-key-down");
        let dir = path.parent().unwrap().to_path_buf();
        let window = cx.add_window(|_window, _cx| gpui::Empty);
        cx.update(|app| {
            app.set_global(ShortcutBindings::with_defaults(path));
        });

        // Conflict: ⌘B is ToggleSidebar's default — capture must NOT commit; it flags
        // the conflict (pending combo + loser) and stays recording (no silent
        // overwrite).
        window
            .update(cx, |_v, window, cx| {
                enter_record(window, cx, ShortcutAction::NewTerminalPane);
                apply_key_down(cx, &key_down("cmd-b"));
            })
            .unwrap();
        cx.update(|app| {
            assert_eq!(
                recording_action(app),
                Some(ShortcutAction::NewTerminalPane),
                "a conflict keeps the recorder recording"
            );
            assert_eq!(pending_combo(app), OwnedCombo::from_token("cmd-b"));
            assert_eq!(conflict_action(app), Some(ShortcutAction::ToggleSidebar));
            assert_eq!(
                app.global::<ShortcutBindings>()
                    .binding(ShortcutAction::NewTerminalPane),
                OwnedCombo::from_token("cmd-t"),
                "the conflict did not silently overwrite the recording action's binding"
            );
        });

        // Commit: ⌘Y is free — capture commits it (set_binding) and tears down.
        window
            .update(cx, |_v, window, cx| {
                enter_record(window, cx, ShortcutAction::NewTerminalPane);
                apply_key_down(cx, &key_down("cmd-y"));
            })
            .unwrap();
        cx.update(|app| {
            assert_eq!(
                recording_action(app),
                None,
                "a free-combo commit tears the capture down"
            );
            assert_eq!(pending_combo(app), None, "commit clears any pending combo");
            assert_eq!(
                app.global::<ShortcutBindings>()
                    .binding(ShortcutAction::NewTerminalPane),
                OwnedCombo::from_token("cmd-y"),
                "the free combo committed to the recording action"
            );
        });

        // Cancel: plain Esc tears down with no change (binding stays ⌘Y).
        window
            .update(cx, |_v, window, cx| {
                enter_record(window, cx, ShortcutAction::NewTerminalPane);
                apply_key_down(cx, &key_down("escape"));
            })
            .unwrap();
        cx.update(|app| {
            assert_eq!(recording_action(app), None, "plain Esc tears the capture down");
            assert_eq!(
                app.global::<ShortcutBindings>()
                    .binding(ShortcutAction::NewTerminalPane),
                OwnedCombo::from_token("cmd-y"),
                "Esc-cancel left the binding untouched"
            );
        });

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `resolve_replace` (§Recorder conflict row → Replace): clear the loser's
    /// binding, bind the recording action to the pending combo, then tear down.
    #[gpui::test]
    fn resolve_replace_unbinds_loser_and_binds_recorder(cx: &mut gpui::TestAppContext) {
        let path = unique_temp_ui_settings("resolve-replace");
        let dir = path.parent().unwrap().to_path_buf();
        let window = cx.add_window(|_window, _cx| gpui::Empty);
        cx.update(|app| {
            app.set_global(ShortcutBindings::with_defaults(path));
        });

        // Record ⌘B onto NewTerminalPane → conflict with ToggleSidebar (its default).
        window
            .update(cx, |_v, window, cx| {
                enter_record(window, cx, ShortcutAction::NewTerminalPane);
                apply_key_down(cx, &key_down("cmd-b"));
            })
            .unwrap();

        cx.update(|app| {
            resolve_replace(app);

            let store = app.global::<ShortcutBindings>();
            assert_eq!(
                store.binding(ShortcutAction::ToggleSidebar),
                None,
                "Replace unbinds the losing action (⌘B is freed from ToggleSidebar)"
            );
            assert_eq!(
                store.binding(ShortcutAction::NewTerminalPane),
                OwnedCombo::from_token("cmd-b"),
                "Replace binds the pending combo to the recording action"
            );
            assert_eq!(
                recording_action(app),
                None,
                "Replace tears the capture down"
            );
        });

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `resolve_replace`'s incomplete-state early return (the line-242 branch):
    /// invoked with no pending capture (recording set, but no pending combo /
    /// conflict), it must NOT rebind anything — it just tears down. Unreachable from
    /// the scenario, which only calls Replace from a fully-populated conflict row.
    #[gpui::test]
    fn resolve_replace_with_no_pending_capture_just_tears_down(cx: &mut gpui::TestAppContext) {
        let path = unique_temp_ui_settings("resolve-replace-incomplete");
        let dir = path.parent().unwrap().to_path_buf();
        let window = cx.add_window(|_window, _cx| gpui::Empty);
        cx.update(|app| {
            app.set_global(ShortcutBindings::with_defaults(path));
        });

        // Enter recording but capture nothing: recording is Some, pending/conflict None.
        window
            .update(cx, |_v, window, cx| {
                enter_record(window, cx, ShortcutAction::NewTerminalPane);
            })
            .unwrap();
        cx.update(|app| {
            assert_eq!(
                recording_action(app),
                Some(ShortcutAction::NewTerminalPane),
                "recording, but with no pending combo yet"
            );
            assert_eq!(pending_combo(app), None);

            resolve_replace(app);

            assert_eq!(
                recording_action(app),
                None,
                "an incomplete Replace still tears the capture down"
            );
            assert!(
                app.global::<ShortcutBindings>()
                    .is_at_default(ShortcutAction::NewTerminalPane),
                "an incomplete Replace rebinds nothing (NewTerminalPane stays at ⌘T)"
            );
        });

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ===================================================================
    // Focus-out teardown (the anti-stranded-keymap safety net)
    // ===================================================================

    /// A window root that tracks the recorder's focus handle (once `enter_record`
    /// installs it) plus a sibling focusable — so the focus PATH can hold the
    /// recorder, then leave it, which is what fires [`Window::on_focus_out`].
    struct FocusOutHost {
        other: FocusHandle,
    }

    impl Render for FocusOutHost {
        fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
            let mut root = div();
            if let Some(recorder) = recorder_focus(cx) {
                root = root.child(div().track_focus(&recorder).w(px(10.0)).h(px(10.0)));
            }
            root.child(div().track_focus(&self.other).w(px(10.0)).h(px(10.0)))
        }
    }

    /// Leaving the recorder mid-capture (rail switch / window close / click-away)
    /// must run `teardown` — the `.onDisappear` guarantee (line ~191). It is the ONLY
    /// mechanism preventing a permanent global keymap outage when a capture is
    /// abandoned without commit / cancel / Esc. Here: focus the recorder via
    /// `enter_record` (keymap stands down), then focus a DIFFERENT handle and assert
    /// the capture tore down AND the full keymap — incl. every PROTECTED
    /// non-rebindable — was restored.
    #[gpui::test]
    fn focus_out_tears_down_capture_and_restores_keymap(cx: &mut gpui::TestAppContext) {
        use gpui::{Action, Keystroke};

        let path = unique_temp_ui_settings("focus-out");
        let dir = path.parent().unwrap().to_path_buf();
        let window = cx.add_window(|_window, cx| FocusOutHost {
            other: cx.focus_handle(),
        });

        // Install the store and seed the live keymap so the stand-down is observable.
        cx.update(|app| {
            app.set_global(ShortcutBindings::with_defaults(path));
            crate::keymap::rebuild_keymap(app);
            assert!(
                app.key_bindings().borrow().bindings().len() > 0,
                "the seeded keymap starts populated"
            );
        });

        // Activate the window: an inactive window reports an empty focus path, so the
        // focus-out would never fire (draw() gates on `previous_window_active`).
        window
            .update(cx, |_host, window, _cx| window.activate_window())
            .unwrap();
        cx.run_until_parked();

        // Enter capture: keymap stands down (D3), recorder is focused.
        window
            .update(cx, |_host, window, cx| {
                enter_record(window, cx, ShortcutAction::NewTerminalPane);
            })
            .unwrap();
        cx.run_until_parked();
        cx.update(|app| {
            assert_eq!(
                recording_action(app),
                Some(ShortcutAction::NewTerminalPane),
                "enter_record put the pane into capture mode"
            );
            assert_eq!(
                app.key_bindings().borrow().bindings().len(),
                0,
                "enter_record stands the whole keymap down while recording"
            );
        });

        // Blur the recorder by focusing a different handle — the focus-out safety net
        // must run teardown.
        window
            .update(cx, |host, window, cx| {
                let other = host.other.clone();
                other.focus(window, cx);
            })
            .unwrap();
        cx.run_until_parked();

        cx.update(|app| {
            assert_eq!(
                recording_action(app),
                None,
                "focus-out tore the abandoned capture down"
            );

            let keymap = app.key_bindings();
            let keymap = keymap.borrow();
            let bound = |action: &dyn Action, chord: &str| -> bool {
                let ks = Keystroke::parse(chord).expect("test chord parses");
                keymap
                    .bindings_for_action(action)
                    .any(|b| matches!(b.match_keystrokes(std::slice::from_ref(&ks)), Some(false)))
            };
            // The full board is back, including every PROTECTED non-rebindable — the
            // capture cannot strand the app with zero shortcuts.
            assert!(bound(&crate::app::ToggleFullScreen, "ctrl-cmd-f"), "⌃⌘F restored");
            assert!(bound(&crate::app::NewWindow, "cmd-n"), "⌘N restored");
            assert!(bound(&crate::app::Quit, "cmd-q"), "⌘Q restored");
            assert!(bound(&crate::app::CloseWindow, "cmd-w"), "⌘W restored");
            assert!(
                bound(&crate::sidebar_shell::CollapseSidebarSelection, "escape"),
                "Esc@SidebarShell restored"
            );
            assert!(
                bound(&crate::settings::window::OpenSettings, "cmd-,"),
                "⌘, restored"
            );
        });

        let _ = std::fs::remove_dir_all(&dir);
    }
}
