//! The in-house, scenario-drivable confirmation modal (W5 / R18).
//!
//! macOS's native `Window::prompt` is not scenario-drivable (seams dossier
//! §10.3), so — like [`crate::context_menu`] — this builds the dialog from gpui
//! primitives: a [`gpui::deferred`] full-window overlay (paints above the whole
//! shell) with a dimmed click-away backdrop and a centered card, focus-grabbed so
//! Esc/Enter reach it. It is a [`gpui::ManagedView`] (`Focusable +
//! EventEmitter<DismissEvent> + Render`): the per-window
//! [`crate::window_state::WindowState`] holds an `Option<Entity<ConfirmationModal>>`,
//! presents it, and drops it on [`gpui::DismissEvent`]; [`crate::app_shell::AppShellView`]
//! renders it while present.
//!
//! ## The generic parameter surface (Exported contract)
//!
//! **`(title, message, confirm_label, cancel_label, destructive_confirm,
//! completion)`** — pinned so R19/R20 reuse it verbatim (R20 supplies a custom
//! `cancel_label` like `"Keep .<old>"`, `destructive_confirm = true` on
//! `"Use .<new>"` / `"Rename Anyway"`, and an async `completion`). R18's own
//! quit/close dialogs are just callers, passing `cancel_label = "Cancel"`,
//! `destructive_confirm = false`; the wording builders live in
//! [`crate::lifecycle`].
//!
//! `completion(confirmed, window, cx)` runs exactly once — on the confirm click
//! / Enter (`true`), or on the cancel click / Esc / click-away (`false`) — before
//! the modal dismisses. R18's callers treat `confirmed == false` as a **total
//! no-op** (Cancel leaves everything untouched); a future caller can act on it.
//!
//! The two buttons carry AX anchors — element ids + `aria_label`s
//! [`CONFIRM_ACCEPT_ID`] / [`CONFIRM_CANCEL_ID`] and [`gpui::Role::Button`] — so a
//! scenario locates them by role + label.

// Presented by `WindowState`; rendered by `AppShellView`. The component's
// constructor / `Render` have no in-crate caller until those wire it (this slice
// wires the quit/close callers; the scenario is slice 3), and R19/R20 reuse the
// surface — the deliberately-exported-component `dead_code` pattern.
#![allow(dead_code)]

use std::rc::Rc;

use gpui::{
    deferred, div, px, App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, Render, Role,
    SharedString, StatefulInteractiveElement, Styled, Window,
};

use nice_theme::chrome_geometry::{CARD_CORNER_RADIUS, INNER_CORNER_RADIUS};
use nice_theme::palette::{slots, ColorScheme, Palette, Slots};

use crate::theme::{slot_to_rgba, srgba_to_rgba, srgba_with_alpha};

/// Selftest instrumentation (Bug B pin): the confirmation-modal backdrop's last
/// PAINTED bounds as `(x, y, width, height)` in window content points, recorded
/// on every paint of the deferred overlay (see [`ConfirmationModal::render`]).
/// The `persistence-restore` scenario reads it via [`modal_backdrop_painted_bounds`]
/// to assert the backdrop rasterizes at ~full window content size — the regression
/// (Bug B) was that, as a flex child of the shell root, the `size_full()` backdrop
/// resolved to empty/zero layout bounds and the deferred paint rasterized nothing
/// while focus/AX still registered in prepaint, so a pixel-blind test passed twice.
/// Recorded only under `selftest`; a constant `None` in the shipped bundle (the
/// probe element is compiled out there), so the reader is always callable but the
/// backing store is `selftest`-only.
#[cfg(feature = "selftest")]
static MODAL_BACKDROP_BOUNDS: std::sync::Mutex<Option<(f32, f32, f32, f32)>> =
    std::sync::Mutex::new(None);

/// Reader for [`MODAL_BACKDROP_BOUNDS`]: the last painted backdrop bounds as
/// `(x, y, width, height)` in window content points, or `None` if the backdrop
/// has not painted this process. A constant `None` outside `selftest`. Always
/// compiled so the always-built scenario module can name it in a plain
/// `cargo build -p nice`.
pub(crate) fn modal_backdrop_painted_bounds() -> Option<(f32, f32, f32, f32)> {
    #[cfg(feature = "selftest")]
    {
        *MODAL_BACKDROP_BOUNDS.lock().unwrap()
    }
    #[cfg(not(feature = "selftest"))]
    {
        None
    }
}

/// The confirm button's stable element id + `aria_label` (the AX anchor a
/// scenario matches on).
pub(crate) const CONFIRM_ACCEPT_ID: &str = "confirm.accept";
/// The cancel button's stable element id + `aria_label`.
pub(crate) const CONFIRM_CANCEL_ID: &str = "confirm.cancel";

/// Card width (pt) — comfortable for a two-line informative message.
const MODAL_WIDTH: f32 = 380.0;
/// Deferred draw priority — above the context menu ([`crate::context_menu`] uses
/// 1000) so a confirmation is never occluded by a stray menu layer.
const MODAL_PRIORITY: usize = 2000;
/// Backdrop dim: black at this alpha over the whole window.
const BACKDROP_ALPHA: f32 = 0.35;
/// Button hover highlight — the `ink` slot at this alpha (the chrome row idiom).
const HOVER_INK_ALPHA: f32 = 0.10;
/// Destructive-confirm fill (a warm red) — used only when `destructive_confirm`
/// is set (R20's "Use .<new>" / "Rename Anyway"). R18's dialogs pass `false`.
const DESTRUCTIVE_RGBA: u32 = 0xC0_39_2B;

/// The confirmation completion: `completion(confirmed, window, cx)`, run once
/// before dismissal. `Rc` so the two button handlers + the key handler can each
/// hold a clone.
pub(crate) type ConfirmationCompletion = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// The confirmation dialog popup. Construct with [`ConfirmationModal::new`]; the
/// owner holds it in an `Option<Entity<ConfirmationModal>>` and subscribes to
/// [`DismissEvent`].
pub(crate) struct ConfirmationModal {
    title: SharedString,
    message: SharedString,
    confirm_label: SharedString,
    cancel_label: SharedString,
    /// Style the confirm button as destructive (red fill). R18 passes `false`.
    destructive_confirm: bool,
    completion: ConfirmationCompletion,
    /// One-shot guard: `completion` + dismissal run at most once (Esc-then-click
    /// / Enter-then-click can't double-fire).
    done: bool,
    focus_handle: FocusHandle,
}

impl ConfirmationModal {
    /// Open a modal with the generic surface, grabbing keyboard focus so
    /// Esc/Enter reach it. Call inside
    /// `cx.new(|cx| ConfirmationModal::new(.., window, cx))`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        title: impl Into<SharedString>,
        message: impl Into<SharedString>,
        confirm_label: impl Into<SharedString>,
        cancel_label: impl Into<SharedString>,
        destructive_confirm: bool,
        completion: impl Fn(bool, &mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        Self {
            title: title.into(),
            message: message.into(),
            confirm_label: confirm_label.into(),
            cancel_label: cancel_label.into(),
            destructive_confirm,
            completion: Rc::new(completion),
            done: false,
            focus_handle,
        }
    }

    /// The modal's title line — the `file-browser` scenario asserts the
    /// rename-confirmation wording through it (a presented modal's identity).
    pub(crate) fn scenario_title(&self) -> String {
        self.title.to_string()
    }

    /// Resolve the modal: run `completion(confirmed)` once, then dismiss.
    /// Idempotent via [`done`](Self::done). `pub(crate)` so the
    /// `persistence-restore` scenario can drive the modal's Cancel / Confirm
    /// answer directly (the plan's hermeticity rule requires only the close
    /// button be a real CGEvent; the modal answer is driven, not real-clicked).
    pub(crate) fn resolve(&mut self, confirmed: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.done {
            return;
        }
        self.done = true;
        (self.completion.clone())(confirmed, window, cx);
        cx.emit(DismissEvent);
    }

    /// The Nice/Dark chrome slot table (mirrors [`crate::context_menu`]).
    fn chrome_slots() -> Slots {
        slots(Palette::Nice, ColorScheme::Dark)
            .expect("Nice + Dark is a valid palette/scheme combo")
    }
}

impl Focusable for ConfirmationModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<DismissEvent> for ConfirmationModal {}

impl Render for ConfirmationModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = Self::chrome_slots();
        let hover = srgba_to_rgba(srgba_with_alpha(
            crate::theme::slot_srgba(s.ink),
            HOVER_INK_ALPHA,
        ));

        // Cancel button — subtle (ink on transparent, hover highlight).
        let cancel = div()
            .id(CONFIRM_CANCEL_ID)
            .role(Role::Button)
            .aria_label(CONFIRM_CANCEL_ID)
            .px_3()
            .py_1()
            .rounded(px(INNER_CORNER_RADIUS))
            .border_1()
            .border_color(slot_to_rgba(s.line))
            .text_color(slot_to_rgba(s.ink))
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .child(self.cancel_label.clone())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, window, cx| {
                    this.resolve(false, window, cx);
                    cx.stop_propagation();
                }),
            );

        // Confirm button — filled. Destructive ⇒ red; else a strong divider fill.
        let confirm_bg = if self.destructive_confirm {
            gpui::rgb(DESTRUCTIVE_RGBA)
        } else {
            slot_to_rgba(s.line_strong)
        };
        let confirm = div()
            .id(CONFIRM_ACCEPT_ID)
            .role(Role::Button)
            .aria_label(CONFIRM_ACCEPT_ID)
            .px_3()
            .py_1()
            .rounded(px(INNER_CORNER_RADIUS))
            .bg(confirm_bg)
            .text_color(slot_to_rgba(s.ink))
            .cursor_pointer()
            .hover(move |style| style.opacity(0.85))
            .child(self.confirm_label.clone())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, window, cx| {
                    this.resolve(true, window, cx);
                    cx.stop_propagation();
                }),
            );

        let card = div()
            .track_focus(&self.focus_handle)
            .key_context("ConfirmationModal")
            .occlude()
            // Enter confirms, Esc cancels (the card holds focus).
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "enter" => {
                        this.resolve(true, window, cx);
                        cx.stop_propagation();
                    }
                    "escape" => {
                        this.resolve(false, window, cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .flex()
            .flex_col()
            .gap_3()
            .w(px(MODAL_WIDTH))
            .p_4()
            .bg(slot_to_rgba(s.panel))
            .border_1()
            .border_color(slot_to_rgba(s.line))
            .rounded(px(CARD_CORNER_RADIUS))
            .shadow_lg()
            .child(
                div()
                    .text_color(slot_to_rgba(s.ink))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(self.title.clone()),
            )
            .child(
                div()
                    .text_color(slot_to_rgba(s.ink2))
                    .child(self.message.clone()),
            )
            .child(
                // Trailing button row: Cancel then Confirm (confirm right-most,
                // the macOS default-button position).
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap_2()
                    .child(cancel)
                    .child(confirm),
            );

        // Full-window dimmed backdrop; a press anywhere on it cancels
        // (click-away). The card centers over it and swallows its own presses.
        //
        // Bug B fix: `absolute().inset_0()` — NOT `size_full()`. The modal is
        // composed as a flex child of the shell's flex root
        // (`AppShellView::render` → `.children(modal)`); a `size_full()` child
        // there resolves to empty/zero layout bounds, so the deferred paint
        // rasterized nothing (focus/AX still registered in prepaint regardless of
        // bounds — which is why the dialog "worked" yet painted zero pixels). An
        // absolutely-positioned, zero-inset overlay stretches to the window's full
        // content box independent of the flex layout, so the backdrop covers the
        // window and the card centers over it (the same bounds-independence the
        // `context_menu` overlay gets from `anchored()`).
        let backdrop = div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(srgba_to_rgba(nice_theme::color::Srgba::new(0.0, 0.0, 0.0, BACKDROP_ALPHA)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, window, cx| {
                    this.resolve(false, window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(card);

        // Selftest instrumentation (Bug B pin): a zero-visual absolute-inset probe
        // that fills the backdrop and records the backdrop's PAINTED bounds into
        // `MODAL_BACKDROP_BOUNDS`. The `persistence-restore` scenario reads it via
        // `modal_backdrop_painted_bounds` and asserts the backdrop rasterized at
        // ~full window content size (non-empty), not the zero bounds Bug B
        // collapsed it to. Compiled out of the shipped bundle.
        #[cfg(feature = "selftest")]
        let backdrop = backdrop.child(
            // The canvas is itself `absolute().inset_0()` so its OWN laid-out
            // bounds equal the backdrop's full content box (a plain, un-sized
            // canvas would collapse to zero on its flex main axis). Recording the
            // canvas's painted bounds thus reports the backdrop's real full-window
            // bounds.
            gpui::canvas(
                |_, _, _| (),
                |bounds, _, _, _| {
                    *MODAL_BACKDROP_BOUNDS.lock().unwrap() = Some((
                        f32::from(bounds.origin.x),
                        f32::from(bounds.origin.y),
                        f32::from(bounds.size.width),
                        f32::from(bounds.size.height),
                    ));
                },
            )
            .absolute()
            .inset_0(),
        );

        deferred(backdrop).with_priority(MODAL_PRIORITY)
    }
}
