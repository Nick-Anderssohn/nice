//! A minimal in-house context-menu component.
//!
//! The pinned gpui has **no** context-menu widget (verified absence), and adding
//! an external UI dependency is a pin decision reserved for a human (plan binding
//! decision "Context menus are built in-house"). So this builds the popup from
//! gpui primitives — [`gpui::anchored`] (position at the click point, flip to
//! stay on-screen) + [`gpui::deferred`] (paint above all ancestors even though
//! it is opened from a deeply-nested row) + right-button mouse handling on the
//! owner — and supports the four things R10/R11 menus need: **items**,
//! **separators**, **disabled** items, and **click-away + Esc** dismissal.
//!
//! ## Ownership / lifecycle (how a row opens one)
//!
//! [`ContextMenu`] is a [`gpui::ManagedView`] (`Focusable + EventEmitter<DismissEvent>
//! + Render`): the owner holds an `Option<Entity<ContextMenu>>`, opens it from a
//! `.on_mouse_down(MouseButton::Right, …)` handler on the row via
//! [`ContextMenu::new`] (which grabs keyboard focus so Esc reaches it), renders
//! it as a child while present, and subscribes to [`gpui::DismissEvent`] to drop
//! it. Slice 3's `TabRow` / `ProjectGroup` wire the right-button trigger and the
//! per-row item sets (e.g. "Close N Tabs", Rename); R11 reuses this component for
//! the toolbar pill menus. This module owns only the reusable popup + item model.

// The component's constructor, `Render`, and the item builders have no in-crate
// caller until slice 3 (TabRow/ProjectGroup) and R11 wire them; it is a
// deliberately-exported reusable component (plan "Exported contracts"). The pure
// item-model accessors below ARE exercised by this module's unit tests.
#![allow(dead_code)]

use std::rc::Rc;

use gpui::{
    anchored, deferred, div, px, App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, Pixels, Point, Render,
    SharedString, Styled, Window,
};

use nice_theme::chrome_geometry::{CARD_CORNER_RADIUS, INNER_CORNER_RADIUS};
use nice_theme::palette::Slots;

use crate::theme::{slot_to_rgba, srgba_to_rgba, srgba_with_alpha};

/// Minimum popup width (pt) so short single-word items still read as a menu.
const CONTEXT_MENU_MIN_WIDTH: f32 = 180.0;
/// Deferred draw priority — high so the menu paints above the sidebar peek
/// overlay and every other sidebar chrome layer (slice 3 keeps those lower).
const CONTEXT_MENU_PRIORITY: usize = 1000;
/// Alpha applied to the `ink` slot for an item's hover highlight — the plan's
/// "hover 6% ink" row convention, reused for menu rows.
const HOVER_INK_ALPHA: f32 = 0.06;

/// A menu action's handler: run on click, before the menu dismisses. Takes the
/// window + app so it can drive the injected `SidebarActions` seam (slice 3).
pub(crate) type MenuHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// One row of a [`ContextMenu`]: a clickable (optionally disabled) entry or a
/// separator.
#[derive(Clone)]
pub(crate) enum ContextMenuItem {
    /// A labeled action row.
    Entry(ContextMenuEntry),
    /// A thin divider between groups of entries.
    Separator,
}

/// A labeled action row of a [`ContextMenu`].
#[derive(Clone)]
pub(crate) struct ContextMenuEntry {
    label: SharedString,
    /// When true the row renders dimmed and ignores clicks.
    disabled: bool,
    /// Run on click (enabled rows only), before dismissal.
    handler: MenuHandler,
}

impl ContextMenuItem {
    /// An enabled action row: `label`, running `handler` on click.
    pub(crate) fn entry(
        label: impl Into<SharedString>,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        ContextMenuItem::Entry(ContextMenuEntry {
            label: label.into(),
            disabled: false,
            handler: Rc::new(handler),
        })
    }

    /// A disabled (dimmed, non-clickable) action row — e.g. the Settings gear
    /// placeholder until R23, or an action not applicable to the current
    /// selection.
    pub(crate) fn disabled(label: impl Into<SharedString>) -> Self {
        ContextMenuItem::Entry(ContextMenuEntry {
            label: label.into(),
            disabled: true,
            handler: Rc::new(|_, _| {}),
        })
    }

    /// A divider row.
    pub(crate) fn separator() -> Self {
        ContextMenuItem::Separator
    }

    /// Whether this item is a separator.
    pub(crate) fn is_separator(&self) -> bool {
        matches!(self, ContextMenuItem::Separator)
    }

    /// The row's label, or `None` for a separator.
    pub(crate) fn label(&self) -> Option<&str> {
        match self {
            ContextMenuItem::Entry(e) => Some(e.label.as_ref()),
            ContextMenuItem::Separator => None,
        }
    }

    /// Whether this item is a clickable (enabled entry) row — separators and
    /// disabled entries are not.
    pub(crate) fn is_enabled(&self) -> bool {
        matches!(self, ContextMenuItem::Entry(e) if !e.disabled)
    }
}

/// The context-menu popup. Construct with [`ContextMenu::new`]; the owner holds
/// it in an `Option<Entity<ContextMenu>>` and subscribes to [`DismissEvent`].
pub(crate) struct ContextMenu {
    items: Vec<ContextMenuItem>,
    /// Window-space anchor — the point the menu opens at (the right-click
    /// position). The [`anchored`] element flips its corner to stay on-screen.
    position: Point<Pixels>,
    focus_handle: FocusHandle,
}

impl ContextMenu {
    /// Open a menu at `position` (window coords, typically the right-click point)
    /// with `items`, grabbing keyboard focus so Esc dismisses it. Call inside
    /// `cx.new(|cx| ContextMenu::new(pos, items, window, cx))`.
    pub(crate) fn new(
        position: Point<Pixels>,
        items: Vec<ContextMenuItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        // Grab focus so the bubble-phase key handler below sees Esc even though
        // the terminal held focus a moment ago.
        focus_handle.focus(window, cx);
        Self {
            items,
            position,
            focus_handle,
        }
    }

    /// Dismiss the menu — emits [`DismissEvent`] for the owner to drop it.
    fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    /// The visible entry labels in order (separators skipped) — a read seam for
    /// self-test scenarios asserting menu contents (e.g. the R19 file-browser
    /// right-click visibility matrix + the Open With ▸ second stage).
    pub(crate) fn item_labels(&self) -> Vec<String> {
        self.items
            .iter()
            .filter_map(|i| i.label().map(str::to_string))
            .collect()
    }

    /// The active chrome slot table the popup paints with — the live
    /// [`SharedThemeState`](crate::theme_settings::SharedThemeState) (Nice/Dark
    /// fallback when the theme global is absent). R21: was a fixed Nice/Dark table.
    fn chrome_slots(cx: &App) -> Slots {
        crate::theme_settings::active_chrome_slots(cx)
    }

    /// Build one item's element. Associated (not `&self`) so the render loop can
    /// borrow `cx` mutably for `cx.listener` without also holding a borrow of
    /// `self`.
    fn render_item(item: &ContextMenuItem, cx: &mut Context<Self>) -> gpui::AnyElement {
        let s = Self::chrome_slots(cx);
        match item {
            ContextMenuItem::Separator => div()
                .h(px(1.0))
                .mx_1()
                .my_1()
                .bg(slot_to_rgba(s.line))
                .into_any_element(),
            ContextMenuItem::Entry(entry) => {
                let row = div()
                    .flex()
                    .items_center()
                    .px_2()
                    .py_1()
                    .rounded(px(INNER_CORNER_RADIUS))
                    .child(entry.label.clone());
                if entry.disabled {
                    // Dimmed, non-interactive.
                    row.text_color(slot_to_rgba(s.ink3)).into_any_element()
                } else {
                    let hover = srgba_to_rgba(srgba_with_alpha(
                        crate::theme::slot_srgba(s.ink),
                        HOVER_INK_ALPHA,
                    ));
                    let handler = entry.handler.clone();
                    row.text_color(slot_to_rgba(s.ink))
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _event, window, cx| {
                                // Run the action, then dismiss. Consume the press
                                // so it never reaches the row/band behind.
                                handler(window, cx);
                                this.dismiss(cx);
                                cx.stop_propagation();
                            }),
                        )
                        .into_any_element()
                }
            }
        }
    }
}

impl Focusable for ContextMenu {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<DismissEvent> for ContextMenu {}

impl Render for ContextMenu {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = Self::chrome_slots(cx);
        let rows: Vec<gpui::AnyElement> = self
            .items
            .iter()
            .map(|item| Self::render_item(item, cx))
            .collect();

        let panel = div()
            .track_focus(&self.focus_handle)
            .key_context("ContextMenu")
            // Capture presses inside the panel's empty areas so they never fall
            // through to the sidebar/terminal behind.
            .occlude()
            // Click-away: a press anywhere outside the panel dismisses it.
            .on_mouse_down_out(cx.listener(|this, _event, _window, cx| this.dismiss(cx)))
            // Esc dismisses (the panel holds focus, so this bubble-phase handler
            // sees it); consume it so it doesn't also reach the terminal.
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                if event.keystroke.key == "escape" {
                    this.dismiss(cx);
                    cx.stop_propagation();
                }
            }))
            .flex()
            .flex_col()
            .min_w(px(CONTEXT_MENU_MIN_WIDTH))
            .py_1()
            .px_1()
            .bg(slot_to_rgba(s.panel))
            .border_1()
            .border_color(slot_to_rgba(s.line))
            .rounded(px(CARD_CORNER_RADIUS))
            .shadow_lg()
            .children(rows);

        // Anchor at the click point (flipping corners to stay on-screen), and
        // defer so the popup paints above all ancestors and sidebar chrome.
        deferred(anchored().position(self.position).child(panel)).with_priority(CONTEXT_MENU_PRIORITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_is_enabled_and_labeled() {
        let item = ContextMenuItem::entry("Close Tab", |_, _| {});
        assert!(!item.is_separator());
        assert!(item.is_enabled());
        assert_eq!(item.label(), Some("Close Tab"));
    }

    #[test]
    fn disabled_entry_is_not_enabled_but_is_labeled() {
        // The Settings gear placeholder pattern (disabled until R23).
        let item = ContextMenuItem::disabled("Settings…");
        assert!(!item.is_separator());
        assert!(!item.is_enabled());
        assert_eq!(item.label(), Some("Settings…"));
    }

    #[test]
    fn separator_has_no_label_and_is_not_enabled() {
        let item = ContextMenuItem::separator();
        assert!(item.is_separator());
        assert!(!item.is_enabled());
        assert_eq!(item.label(), None);
    }

    #[test]
    fn a_dynamic_close_n_label_round_trips() {
        // Slice 3 builds the "Close N Tabs" label from the selection size; the
        // item model carries whatever label it is handed.
        let item = ContextMenuItem::entry("Close 3 Tabs", |_, _| {});
        assert_eq!(item.label(), Some("Close 3 Tabs"));
        assert!(item.is_enabled());
    }
}
