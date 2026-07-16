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
    anchored, deferred, div, prelude::*, px, App, Context, DismissEvent, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, Pixels,
    Point, Render, SharedString, StatefulInteractiveElement, Styled, Window,
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
/// Menu row text point size at the 12pt chrome-font anchor — parity with the
/// 13pt native NSMenu font every prod menu (sidebar/toolbar/file-browser
/// context menus, settings pop-up buttons) renders with.
const MENU_TEXT_BASE: f32 = 13.0;

/// The menu's text size: the 13pt NSMenu base scaled by the user's chrome
/// (sidebar) font setting — the same settings source the sidebar chrome reads
/// ([`crate::settings::sidebar_font`]). Without an explicit size the popup
/// inherited the window default (16px), reading far larger than the chrome
/// around it.
fn menu_text_px(sidebar_px: f32) -> f32 {
    crate::settings::sidebar_font::sidebar_size(sidebar_px, MENU_TEXT_BASE)
}

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
    /// `Some(is_selected)` for selectable rows (the settings-dropdown use): the
    /// row renders a leading checkmark column ("✓" when selected, blank
    /// otherwise, so labels stay aligned). `None` for plain action rows.
    selected: Option<bool>,
    /// A stable a11y id for the row (dropdown menu items carry one; plain
    /// context-menu rows do not).
    id: Option<SharedString>,
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
            selected: None,
            id: None,
        })
    }

    /// A selectable (dropdown-style) row: a stable a11y `id`, a leading
    /// checkmark column reflecting `selected`, running `handler` on click.
    pub(crate) fn selectable(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        selected: bool,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        ContextMenuItem::Entry(ContextMenuEntry {
            label: label.into(),
            disabled: false,
            handler: Rc::new(handler),
            selected: Some(selected),
            id: Some(id.into()),
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
            selected: None,
            id: None,
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

    /// `Some(is_selected)` for a selectable (dropdown-style) row; `None` for a
    /// plain entry or separator.
    pub(crate) fn selected(&self) -> Option<bool> {
        match self {
            ContextMenuItem::Entry(e) => e.selected,
            ContextMenuItem::Separator => None,
        }
    }

    /// The row's stable a11y id, when it carries one (selectable rows).
    pub(crate) fn entry_id(&self) -> Option<&str> {
        match self {
            ContextMenuItem::Entry(e) => e.id.as_deref(),
            ContextMenuItem::Separator => None,
        }
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
    /// Minimum popup width — defaults to [`CONTEXT_MENU_MIN_WIDTH`]; a settings
    /// dropdown passes its trigger width so the menu never reads narrower than
    /// the button that opened it.
    min_width: Pixels,
    /// When set, the panel caps its height and the item list scrolls (long
    /// dropdown lists, e.g. every installed font family).
    max_height: Option<Pixels>,
    /// Which corner of the menu sits at `position` (default top-left — the
    /// context-menu click point). A settings dropdown passes top-right with the
    /// trigger's bottom-right so the menu right-aligns to its button, the
    /// NSPopUpButton attachment. `anchored` still flips it to stay on-screen.
    anchor_corner: gpui::Anchor,
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
            min_width: px(CONTEXT_MENU_MIN_WIDTH),
            max_height: None,
            anchor_corner: gpui::Anchor::TopLeft,
        }
    }

    /// Widen the minimum popup width (never below the component default).
    pub(crate) fn with_min_width(mut self, width: Pixels) -> Self {
        self.min_width = self.min_width.max(width);
        self
    }

    /// Cap the panel height; overflowing items scroll.
    pub(crate) fn with_max_height(mut self, height: Pixels) -> Self {
        self.max_height = Some(height);
        self
    }

    /// Put this corner of the menu at the anchor position (see `anchor_corner`).
    pub(crate) fn with_anchor_corner(mut self, corner: gpui::Anchor) -> Self {
        self.anchor_corner = corner;
        self
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
                let mut row = div()
                    .flex()
                    .items_center()
                    .px_2()
                    .py_1()
                    .rounded(px(INNER_CORNER_RADIUS));
                // Selectable rows carry a fixed leading checkmark column so the
                // labels of selected and unselected options stay aligned.
                if let Some(selected) = entry.selected {
                    row = row.child(
                        div()
                            .w(px(16.0))
                            .flex_none()
                            .child(SharedString::from(if selected { "✓" } else { "" })),
                    );
                }
                let row = row.child(entry.label.clone());
                if entry.disabled {
                    // Dimmed, non-interactive.
                    row.text_color(slot_to_rgba(s.ink3)).into_any_element()
                } else {
                    let hover = srgba_to_rgba(srgba_with_alpha(
                        crate::theme::slot_srgba(s.ink),
                        HOVER_INK_ALPHA,
                    ));
                    let handler = entry.handler.clone();
                    // Run the action, then dismiss. Consume the press so it
                    // never reaches the row/band behind.
                    let on_down =
                        cx.listener(move |this, _event: &gpui::MouseDownEvent, window, cx| {
                            handler(window, cx);
                            this.dismiss(cx);
                            cx.stop_propagation();
                        });
                    let row = row
                        .text_color(slot_to_rgba(s.ink))
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover));
                    match entry.id.clone() {
                        // Dropdown items surface a stable a11y id + menu-item role.
                        Some(id) => row
                            .id(id)
                            .role(gpui::Role::MenuItem)
                            .aria_label(entry.label.clone())
                            .on_mouse_down(MouseButton::Left, on_down)
                            .into_any_element(),
                        None => row
                            .on_mouse_down(MouseButton::Left, on_down)
                            .into_any_element(),
                    }
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

        let mut panel = div()
            .id("context-menu.panel")
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
            .min_w(self.min_width)
            .py_1()
            .px_1()
            // The chrome font (13pt NSMenu parity, scaled by the sidebar-font
            // setting) — otherwise rows inherit the 16px window default.
            .text_size(px(menu_text_px(
                crate::settings::sidebar_font::current_sidebar_px(cx),
            )))
            // The chrome family (the restyle's mono chrome look) — otherwise
            // rows inherit the system-font window default.
            .when_some(crate::theme_settings::chrome_font_family(cx), |d, fam| {
                d.font_family(fam)
            })
            .bg(slot_to_rgba(s.panel))
            .border_1()
            .border_color(slot_to_rgba(s.line))
            .rounded(px(CARD_CORNER_RADIUS))
            .shadow_lg();
        if let Some(max_height) = self.max_height {
            panel = panel.max_h(max_height).overflow_y_scroll();
        }
        let panel = panel.children(rows);

        // Anchor at the click point (flipping corners to stay on-screen), and
        // defer so the popup paints above all ancestors and sidebar chrome.
        deferred(
            anchored()
                .position(self.position)
                .anchor(self.anchor_corner)
                .child(panel),
        )
        .with_priority(CONTEXT_MENU_PRIORITY)
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
    fn selectable_entry_carries_id_selected_flag_and_label() {
        let on = ContextMenuItem::selectable("picker.opt-a", "Option A", true, |_, _| {});
        assert!(on.is_enabled());
        assert_eq!(on.label(), Some("Option A"));
        assert_eq!(on.selected(), Some(true));
        assert_eq!(on.entry_id(), Some("picker.opt-a"));

        let off = ContextMenuItem::selectable("picker.opt-b", "Option B", false, |_, _| {});
        assert_eq!(off.selected(), Some(false));
        assert_eq!(off.entry_id(), Some("picker.opt-b"));
    }

    #[test]
    fn plain_entries_and_separators_are_not_selectable_and_carry_no_id() {
        assert_eq!(ContextMenuItem::entry("Close Tab", |_, _| {}).selected(), None);
        assert_eq!(ContextMenuItem::entry("Close Tab", |_, _| {}).entry_id(), None);
        assert_eq!(ContextMenuItem::disabled("Settings…").selected(), None);
        assert_eq!(ContextMenuItem::separator().selected(), None);
        assert_eq!(ContextMenuItem::separator().entry_id(), None);
    }

    #[test]
    fn menu_text_matches_nsmenu_at_default_and_tracks_the_chrome_font_setting() {
        // At the 12pt sidebar-font anchor the menu renders at the 13pt native
        // NSMenu size (prod parity) — NOT the 16px window default.
        assert_eq!(menu_text_px(12.0), 13.0);
        // A resized chrome font scales the menu proportionally with it.
        assert_eq!(menu_text_px(24.0), 26.0);
        assert_eq!(menu_text_px(8.0), 9.0); // 8*13/12 = 8.67 → round 9
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
