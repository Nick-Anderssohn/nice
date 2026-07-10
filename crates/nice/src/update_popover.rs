//! The trailing update-pill's popover (R27, P7 / D9) — a small dedicated anchored
//! card that renders the two Homebrew commands a user runs to upgrade, ported from
//! `Sources/Nice/Views/UpdateAvailablePill.swift` (the popover half, `:76-171`).
//!
//! None of the three existing popup components is a drop-in (dossier surprise #9):
//! [`crate::context_menu`] renders label rows only and its item enum is closed;
//! [`crate::confirmation_modal`] is a centered, non-anchored card. So — exactly as
//! those two build their popups from gpui primitives — this is a small
//! [`gpui::ManagedView`] (`Focusable + EventEmitter<DismissEvent> + Render`)
//! presented via `deferred(anchored().position(..).anchor(..).child(card))`
//! anchored under the pill (Swift `arrowEdge: .bottom`), with click-away + Esc
//! dismissal. The toolbar owns the `Option<Entity<UpdatePopover>>` and subscribes
//! to [`gpui::DismissEvent`] to drop it (the `context_menu` field pattern).
//!
//! ## The frozen copy (parity depends on these strings)
//! * Header: the accent up-arrow glyph + `"Update available: <version>"` where
//!   `<version>` is the latest tag with a leading `v`/`V` stripped; a nil/empty
//!   version renders the bare `"Update available"`.
//! * Two command rows, EXACT strings in order: `brew update`, then
//!   `brew upgrade --cask <cask>` (the cask from [`crate::release_check::CASK_NAME`]).
//!   Each row is monospace command text + a Copy button; Copy writes the command to
//!   the clipboard and flips its label to `"Copied"` for 1.5 s (D8) then back.
//! * Footer: `"Restart Nice after upgrading."` (secondary ink).

// Presented by `WindowToolbarView`; rendered as its child while open. The
// component's constructor / `Render` have no in-crate caller until the toolbar
// pill wires them (same slice) and the `update-check` scenario drives them — the
// deliberately-exported-component `dead_code` pattern the sibling popups use.
#![allow(dead_code)]

use std::time::Duration;

use gpui::{
    anchored, deferred, div, px, App, ClipboardItem, Context, DismissEvent, EventEmitter,
    FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    ParentElement, Pixels, Point, Render, SharedString, StatefulInteractiveElement, Styled, Window,
};

use nice_theme::chrome_geometry::{CARD_CORNER_RADIUS, INNER_CORNER_RADIUS};
use nice_theme::palette::Slots;

use crate::release_check::CASK_NAME;
use crate::sf_symbols::{sf_symbol_icon, SymbolWeight};
use crate::theme::{slot_srgba, slot_to_rgba, srgba_to_rgba, srgba_with_alpha};

/// Fixed popover width (pt) — `UpdateAvailablePill.swift:78`.
const POPOVER_WIDTH: f32 = 320.0;
/// Card padding (pt) — `UpdateAvailablePill.swift:79`.
const POPOVER_PAD: f32 = 16.0;
/// Deferred draw priority — above the context menu (1000) so the popover is never
/// occluded by a stray menu layer, below the confirmation modal (2000).
const POPOVER_PRIORITY: usize = 1500;
/// How long the Copy button reads `"Copied"` before reverting (D8,
/// `UpdateAvailablePill.swift:160`).
const COPY_REVERT: Duration = Duration::from_millis(1500);
/// Copy-button hover fill: the `ink` slot at this alpha (the chrome row idiom).
const HOVER_INK_ALPHA: f32 = 0.08;
/// Command text point size (`UpdateAvailablePill.swift:128`).
const COMMAND_TEXT_SIZE: f32 = 12.0;
/// A stock, always-present macOS monospace family for the command text (the same
/// choice the term-render fixture makes for a font-independent monospace).
const COMMAND_FONT: &str = "Menlo";
/// The accent header glyph — the same SF Symbol the pill leads with, one step
/// larger (`UpdateAvailablePill.swift:96`).
const HEADER_ICON_SF: &str = "arrow.up.circle.fill";
const HEADER_ICON_FALLBACK: &str = "\u{2191}"; // ↑
const HEADER_ICON_SIZE: f32 = 14.0;

/// The Copy button's resting label.
pub(crate) const COPY_LABEL: &str = "Copy";
/// The Copy button's post-click label (reverts after [`COPY_REVERT`]).
pub(crate) const COPIED_LABEL: &str = "Copied";
/// The popover footer (secondary ink) — `UpdateAvailablePill.swift:86`.
const FOOTER_TEXT: &str = "Restart Nice after upgrading.";

/// The header title for `latest_version`: `"Update available: <version>"` with a
/// leading `v`/`V` stripped from `<version>`; a `None`/empty version yields the
/// bare `"Update available"` (`UpdateAvailablePill.swift:104-112`).
fn header_title(latest_version: Option<&str>) -> String {
    let stripped = latest_version
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.strip_prefix(['v', 'V']).unwrap_or(v));
    match stripped {
        Some(v) => format!("Update available: {v}"),
        None => "Update available".to_string(),
    }
}

/// The two brew commands, EXACT strings in order (`UpdateAvailablePill.swift:80-83`).
fn brew_commands() -> Vec<String> {
    vec![
        "brew update".to_string(),
        format!("brew upgrade --cask {CASK_NAME}"),
    ]
}

/// The small anchored popover under the update pill. Construct with
/// [`UpdatePopover::new`]; the toolbar holds it in an `Option<Entity<UpdatePopover>>`
/// and subscribes to [`DismissEvent`].
pub(crate) struct UpdatePopover {
    /// Window-space anchor — the pill click point the popover opens under. The
    /// [`anchored`] element flips its corner to stay on-screen.
    position: Point<Pixels>,
    /// The header title line (already resolved from `latest_version`).
    title: SharedString,
    /// The two brew command strings, in order.
    commands: Vec<SharedString>,
    /// Which command row currently reads `"Copied"` (D8), if any.
    copied: Option<usize>,
    /// Monotonic copy id — a later copy invalidates an earlier row's pending
    /// revert so a stale timer can't wipe a fresh `"Copied"`.
    copy_gen: u64,
    focus_handle: FocusHandle,
}

impl UpdatePopover {
    /// Open the popover at `position` (window coords, the pill click point) for
    /// `latest_version`, grabbing keyboard focus so Esc dismisses it. Call inside
    /// `cx.new(|cx| UpdatePopover::new(pos, latest, window, cx))`.
    pub(crate) fn new(
        position: Point<Pixels>,
        latest_version: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        // Grab focus so the bubble-phase key handler sees Esc even though the
        // terminal held focus a moment ago (the context-menu discipline).
        focus_handle.focus(window, cx);
        Self {
            position,
            title: header_title(latest_version).into(),
            commands: brew_commands().into_iter().map(Into::into).collect(),
            copied: None,
            copy_gen: 0,
            focus_handle,
        }
    }

    /// Dismiss the popover — emits [`DismissEvent`] for the owner to drop it.
    fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    /// Copy command row `index` to the clipboard and flash its Copy button to
    /// `"Copied"` for [`COPY_REVERT`] (D8). The clipboard write is the mandatory
    /// half; the timed revert is a parity nicety. `pub(crate)` so the
    /// `update-check` scenario drives a Copy deterministically (the clipboard
    /// assertion) without a synthetic click on the button.
    pub(crate) fn copy_command(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(cmd) = self.commands.get(index).cloned() else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(cmd.to_string()));
        self.copy_gen = self.copy_gen.wrapping_add(1);
        let gen = self.copy_gen;
        self.copied = Some(index);
        cx.notify();
        // Revert the label after 1.5 s — nap-safe so an occluded window still
        // fires it. A newer copy bumps `copy_gen`, so this revert is a no-op if it
        // has been superseded.
        cx.spawn(async move |this, acx| {
            crate::platform::nap_safe_delay(COPY_REVERT).await;
            let _ = this.update(acx, |this, cx| {
                if this.copy_gen == gen {
                    this.copied = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The two brew command strings — a read seam the `update-check` scenario
    /// asserts against (both exact commands present, in order).
    pub(crate) fn scenario_commands(&self) -> Vec<String> {
        self.commands.iter().map(ToString::to_string).collect()
    }

    /// The header title — a read seam for the scenario / diagnostics.
    pub(crate) fn scenario_title(&self) -> String {
        self.title.to_string()
    }

    /// The active chrome slot table the popover paints with — the live theme
    /// state (the `context_menu` / `confirmation_modal` idiom).
    fn chrome_slots(cx: &App) -> Slots {
        crate::theme_settings::active_chrome_slots(cx)
    }

    /// Build one command row: monospace command text (leading, selectable) + a
    /// trailing Copy button. Associated (not `&self`) so the caller can borrow
    /// `cx` mutably for `cx.listener` without also holding a borrow of `self`.
    fn render_command_row(
        &self,
        index: usize,
        s: &Slots,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let command = self.commands[index].clone();
        let hover = srgba_to_rgba(srgba_with_alpha(slot_srgba(s.ink), HOVER_INK_ALPHA));
        let copied = self.copied == Some(index);
        let button_label = if copied { COPIED_LABEL } else { COPY_LABEL };
        // Copy-button AX label: `"Copy <command>"` (`UpdateAvailablePill.swift:150`).
        let copy_ax = format!("Copy {command}");

        let copy_button = div()
            .id(SharedString::from(format!("update.copy.{index}")))
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(copy_ax))
            .flex_none()
            .px_2()
            .py_1()
            .rounded(px(INNER_CORNER_RADIUS))
            .border_1()
            .border_color(slot_to_rgba(s.line))
            .text_color(slot_to_rgba(s.ink))
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .child(SharedString::from(button_label))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e: &gpui::MouseDownEvent, _window, cx| {
                    this.copy_command(index, cx);
                    // Keep the press inside the popover — never fall through to the
                    // click-away / band behind.
                    cx.stop_propagation();
                }),
            );

        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_2()
            .w_full()
            .child(
                div()
                    .flex_1()
                    .font_family(COMMAND_FONT)
                    .text_size(px(COMMAND_TEXT_SIZE))
                    .text_color(slot_to_rgba(s.ink))
                    .child(command),
            )
            .child(copy_button)
            .into_any_element()
    }
}

impl Focusable for UpdatePopover {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<DismissEvent> for UpdatePopover {}

impl Render for UpdatePopover {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = Self::chrome_slots(cx);
        let scale = window.scale_factor();
        let accent = srgba_to_rgba(crate::theme_settings::active_chrome_accent(cx));

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(sf_symbol_icon(
                HEADER_ICON_SF,
                HEADER_ICON_FALLBACK,
                HEADER_ICON_SIZE,
                SymbolWeight::Semibold,
                accent,
                scale,
                cx,
            ))
            .child(
                div()
                    .text_color(slot_to_rgba(s.ink))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(self.title.clone()),
            );

        let rows: Vec<gpui::AnyElement> = (0..self.commands.len())
            .map(|i| self.render_command_row(i, &s, cx))
            .collect();

        let footer = div()
            .text_color(slot_to_rgba(s.ink2))
            .child(SharedString::from(FOOTER_TEXT));

        let card = div()
            .id("update.popover.panel")
            .track_focus(&self.focus_handle)
            .key_context("UpdatePopover")
            // Capture presses inside the card so they never fall through behind.
            .occlude()
            // Click-away dismisses.
            .on_mouse_down_out(cx.listener(|this, _event, _window, cx| this.dismiss(cx)))
            // Esc dismisses (the card holds focus); consume it.
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                if event.keystroke.key == "escape" {
                    this.dismiss(cx);
                    cx.stop_propagation();
                }
            }))
            .flex()
            .flex_col()
            .gap_3()
            .items_start()
            .w(px(POPOVER_WIDTH))
            .p(px(POPOVER_PAD))
            .bg(slot_to_rgba(s.panel))
            .border_1()
            .border_color(slot_to_rgba(s.line))
            .rounded(px(CARD_CORNER_RADIUS))
            .shadow_lg()
            .child(header)
            .children(rows)
            .child(footer);

        // Anchor under the pill click point (flipping to stay on-screen), and
        // defer so the popover paints above all ancestors (the context-menu
        // wrapping order: `deferred` wraps `anchored`, not the reverse).
        deferred(
            anchored()
                .position(self.position)
                .anchor(gpui::Anchor::TopRight)
                .child(card),
        )
        .with_priority(POPOVER_PRIORITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_title_strips_leading_v_and_prefixes() {
        assert_eq!(header_title(Some("v0.1.5")), "Update available: 0.1.5");
        assert_eq!(header_title(Some("V2.0.0")), "Update available: 2.0.0");
        // No leading v — kept verbatim.
        assert_eq!(header_title(Some("0.1.5")), "Update available: 0.1.5");
    }

    #[test]
    fn header_title_bare_when_version_absent_or_empty() {
        assert_eq!(header_title(None), "Update available");
        assert_eq!(header_title(Some("")), "Update available");
        assert_eq!(header_title(Some("   ")), "Update available");
    }

    #[test]
    fn brew_commands_are_the_two_exact_strings_in_order() {
        let cmds = brew_commands();
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0], "brew update");
        // The cask comes from the frozen constant.
        assert_eq!(cmds[1], "brew upgrade --cask nice");
    }
}
