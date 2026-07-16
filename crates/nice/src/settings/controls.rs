//! Shared Settings-pane controls: the macOS-style [`toggle_switch`], the
//! NSPopUpButton-style [`dropdown`], and the continuous [`slider`] — the compact
//! right-aligned controls the prod (Swift) settings window rendered with native
//! AppKit widgets.
//!
//! ## Slider
//! The mock's `.mini-slider`: a flat 170×3 track with a round `ink` thumb and a
//! value readout on the right. Mouse-down jumps to the pressed position and arms
//! a drag on the owning [`SettingsRootView`], whose root-level mouse listeners
//! keep applying the value under the pointer until release (live preview); the
//! caller's `apply_*` mutator clamps and persists every position.
//!
//! ## Toggle switch
//! A track + thumb switch (accent track / thumb right when on, dimmed-ink track /
//! thumb left when off). A pure element builder — each call site keeps its exact
//! mutator wiring and a11y id, so the "click → the exact `apply_*` call + the
//! exact a11y id" contract is unchanged from the old On/Off pill.
//!
//! ## Dropdown
//! A bordered trigger button showing the CURRENT selection's label with a small
//! up/down-chevron glyph (the `chevron.up.chevron.down` SF Symbol), opening a
//! [`ContextMenu`](crate::context_menu::ContextMenu) popup of selectable rows
//! (checkmark on the selected one) anchored under the trigger. The popup reuses
//! the in-house context-menu machinery (`anchored` + `deferred` + click-away/Esc
//! dismissal); the OPEN state lives on the owning
//! [`SettingsRootView`](crate::settings::root::SettingsRootView) — the panes stay
//! stateless free functions, they just build triggers against the root view's
//! `Context` (see [`SettingsRootView::toggle_dropdown`]).
//!
//! The trigger's window-space bounds are captured by an absolute
//! [`gpui::canvas`] child on every prepaint; the click handler reads the cell to
//! anchor the menu at the trigger's bottom-left (the menu also inherits the
//! trigger width as its minimum, so it never reads narrower than its button).

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    canvas, div, prelude::*, px, AnyElement, App, Bounds, Context, FontWeight, MouseButton,
    Pixels, SharedString, Window,
};

use crate::context_menu::ContextMenuItem;
use crate::settings::root::SettingsRootView;
use crate::sf_symbols::{sf_symbol_icon, SymbolWeight};
use crate::theme::{slot_to_rgba, srgba_to_rgba};
use crate::theme_settings;

// -- toggle switch ------------------------------------------------------------

/// Switch geometry — the mock's flat toggle (`docs/design/restyle-mocks.html`
/// `.toggle`: a 32×18 track, radius 9, a 14pt thumb inset 2px).
const SWITCH_TRACK_WIDTH: f32 = 32.0;
const SWITCH_TRACK_HEIGHT: f32 = 18.0;
const SWITCH_THUMB_SIZE: f32 = 14.0;

/// A flat track+thumb toggle switch (restyle plan 6): accent track with a white
/// thumb right when `on`, an over-glass `--fill-x` track with an `ink3` thumb
/// left when off (the mock's `.toggle` / `.toggle.on`). Carries the caller's
/// a11y `id` + a Switch role + an "On"/"Off" aria label; the click runs
/// `on_click` on `&mut App` — the call site keeps its exact mutator wiring.
pub(crate) fn toggle_switch(
    id: impl Into<SharedString>,
    on: bool,
    cx: &App,
    on_click: impl Fn(&mut App) + 'static,
) -> impl IntoElement {
    let accent = srgba_to_rgba(theme_settings::active_chrome_accent(cx));
    let slots = theme_settings::active_chrome_slots(cx);
    let scheme = theme_settings::active_chrome_scheme(cx);
    // Flat off-track: the over-glass `--fill-x` value (reads on the translucent
    // surface without an opaque fill), not a palette slot.
    let off_track = srgba_to_rgba(nice_theme::glass::glass_fill_x(scheme));
    // The thumb: white when on (accent track), the dimmed `ink3` slot when off.
    let thumb = if on {
        gpui::rgb(0xff_ff_ff)
    } else {
        slot_to_rgba(slots.ink3)
    };
    div()
        .id(id.into())
        .role(gpui::Role::Switch)
        .aria_label(if on { "On" } else { "Off" })
        .flex_none()
        .w(px(SWITCH_TRACK_WIDTH))
        .h(px(SWITCH_TRACK_HEIGHT))
        .rounded(px(SWITCH_TRACK_HEIGHT / 2.0))
        .p(px((SWITCH_TRACK_HEIGHT - SWITCH_THUMB_SIZE) / 2.0))
        .flex()
        .items_center()
        .when(on, |d| d.bg(accent).justify_end())
        .when(!on, |d| d.bg(off_track).justify_start())
        .cursor_pointer()
        .child(
            div()
                .size(px(SWITCH_THUMB_SIZE))
                .rounded(px(SWITCH_THUMB_SIZE / 2.0))
                .bg(thumb),
        )
        .on_mouse_down(MouseButton::Left, move |_e, _window, cx: &mut App| {
            on_click(cx);
        })
}

// -- slider ---------------------------------------------------------------------

/// Slider geometry — the mock's `.mini-slider` (a 170×3 track, a 12pt round
/// thumb) inside a taller invisible hit strip so the 3px track is clickable.
const SLIDER_TRACK_WIDTH: f32 = 170.0;
const SLIDER_TRACK_HEIGHT: f32 = 3.0;
const SLIDER_THUMB_SIZE: f32 = 12.0;
const SLIDER_HIT_HEIGHT: f32 = 16.0;

/// Map a window-space `x` on a slider track to the value it picks — the pure
/// half of the drag math (clamped to the track, linear across `[min, max]`).
pub(crate) fn slider_value_at(x: f32, track_x: f32, track_w: f32, min: f32, max: f32) -> f32 {
    let t = ((x - track_x) / track_w).clamp(0.0, 1.0);
    min + t * (max - min)
}

/// A continuous drag slider (the mock's `.mini-slider` + `.val` readout): a flat
/// `--fill-x` track with an `ink` thumb at the current value and the readout in
/// `ink` to the right. Mouse-down anywhere on the track jumps there and arms a
/// drag on the owning [`SettingsRootView`] (whose root-level mouse listeners
/// track the pointer until release — the sidebar-resize pattern); every position
/// change runs `apply(cx, value)` live. The caller's mutator clamps/rounds
/// authoritatively, so raw f32 track values are handed over as-is.
pub(crate) fn slider(
    a11y: impl Into<SharedString>,
    readout: impl Into<SharedString>,
    value: f32,
    min: f32,
    max: f32,
    cx: &mut Context<SettingsRootView>,
    apply: impl Fn(&mut App, f32) + 'static,
) -> AnyElement {
    let a11y = a11y.into();
    let readout: SharedString = readout.into();
    let slots = theme_settings::active_chrome_slots(cx);
    let scheme = theme_settings::active_chrome_scheme(cx);
    let ink = slot_to_rgba(slots.ink);
    let track_fill = srgba_to_rgba(nice_theme::glass::glass_fill_x(scheme));
    let apply: Rc<dyn Fn(&mut App, f32)> = Rc::new(apply);

    // Thumb center at `t` of the track width (the mock's `left: N%` +
    // `translate(-50%)` — the thumb may overhang the track ends by half).
    let t = if max > min { ((value - min) / (max - min)).clamp(0.0, 1.0) } else { 0.0 };
    let thumb_left = t * SLIDER_TRACK_WIDTH - SLIDER_THUMB_SIZE / 2.0;

    // The track's window-space bounds, refreshed every prepaint (the dropdown
    // trigger's canvas pattern) — mouse-down reads it to seed the drag.
    let track_bounds: Rc<Cell<Bounds<Pixels>>> = Rc::new(Cell::new(Bounds::default()));
    let write_bounds = track_bounds.clone();

    div()
        .id(a11y)
        .role(gpui::Role::Slider)
        .aria_label(readout.clone())
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .child(
            div()
                .relative()
                .flex_none()
                .w(px(SLIDER_TRACK_WIDTH))
                .h(px(SLIDER_HIT_HEIGHT))
                .flex()
                .items_center()
                .cursor_pointer()
                .child(
                    div()
                        .w_full()
                        .h(px(SLIDER_TRACK_HEIGHT))
                        .rounded(px(SLIDER_TRACK_HEIGHT / 2.0))
                        .bg(track_fill),
                )
                .child(
                    div()
                        .absolute()
                        .top(px((SLIDER_HIT_HEIGHT - SLIDER_THUMB_SIZE) / 2.0))
                        .left(px(thumb_left))
                        .size(px(SLIDER_THUMB_SIZE))
                        .rounded(px(SLIDER_THUMB_SIZE / 2.0))
                        .bg(ink),
                )
                .child(
                    canvas(move |bounds, _window, _cx| write_bounds.set(bounds), |_, _, _, _| {})
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, e: &gpui::MouseDownEvent, _window, cx| {
                        this.begin_slider_drag(
                            track_bounds.get(),
                            min,
                            max,
                            apply.clone(),
                            e.position.x,
                            cx,
                        );
                        cx.stop_propagation();
                    }),
                ),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(ink)
                .child(readout),
        )
        .into_any_element()
}

// -- dropdown -------------------------------------------------------------------

/// The vertical gap between the trigger's bottom edge and the menu.
const DROPDOWN_MENU_GAP: f32 = 4.0;
/// The menu's height cap — longer option lists (every installed font family)
/// scroll inside the popup.
const DROPDOWN_MENU_MAX_HEIGHT: f32 = 360.0;
/// The chevron glyph's point size (the NSPopUpButton indicator scale).
const DROPDOWN_CHEVRON_POINT_SIZE: f32 = 8.5;

/// One option of a [`dropdown`]: a stable a11y `id`, a display `label`, whether
/// it is the CURRENT selection (checkmarked in the menu), and the `on_select`
/// handler (the exact `apply_*` call the old chip carried).
#[derive(Clone)]
pub(crate) struct DropdownItem {
    pub(crate) id: SharedString,
    pub(crate) label: SharedString,
    pub(crate) selected: bool,
    on_select: Rc<dyn Fn(&mut App)>,
}

impl DropdownItem {
    pub(crate) fn new(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        selected: bool,
        on_select: impl Fn(&mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            selected,
            on_select: Rc::new(on_select),
        }
    }

    /// Run the option's `apply_*` handler — the click path, exposed so tests can
    /// pin "selection → the exact `apply_*` call" without a window.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn select(&self, cx: &mut App) {
        (self.on_select)(cx);
    }

    /// The menu row this option renders as: a selectable
    /// [`ContextMenuItem`] carrying the option's a11y id + checkmark state.
    pub(crate) fn into_menu_item(self) -> ContextMenuItem {
        let on_select = self.on_select;
        ContextMenuItem::selectable(self.id, self.label, self.selected, move |_window, app| {
            on_select(app)
        })
    }
}

/// The NSPopUpButton-style dropdown trigger: a bordered rounded button showing
/// `current_label` + an up/down chevron, right-alignable in a `setting_row`.
/// Clicking toggles the option menu open under the trigger (owned by the
/// [`SettingsRootView`] — see the module docs). The trigger carries the caller's
/// stable a11y id; each option row carries its own (`DropdownItem::id`).
pub(crate) fn dropdown(
    trigger_id: impl Into<SharedString>,
    current_label: impl Into<SharedString>,
    items: Vec<DropdownItem>,
    window: &mut Window,
    cx: &mut Context<SettingsRootView>,
) -> AnyElement {
    let trigger_id: SharedString = trigger_id.into();
    let current_label: SharedString = current_label.into();
    let slots = theme_settings::active_chrome_slots(cx);
    let ink = slot_to_rgba(slots.ink);
    let ink2 = slot_to_rgba(slots.ink2);
    let line = slot_to_rgba(slots.line);
    let scale = window.scale_factor();
    let chevron = sf_symbol_icon(
        "chevron.up.chevron.down",
        "⇕",
        DROPDOWN_CHEVRON_POINT_SIZE,
        SymbolWeight::Semibold,
        ink2,
        scale,
        cx,
    );

    // The trigger's window-space bounds, refreshed by the canvas child on every
    // prepaint (so it tracks scrolling); the click handler reads it to anchor
    // the menu under the trigger.
    let trigger_bounds: Rc<Cell<Bounds<Pixels>>> = Rc::new(Cell::new(Bounds::default()));
    let write_bounds = trigger_bounds.clone();
    let click_id = trigger_id.clone();

    div()
        .id(trigger_id)
        .role(gpui::Role::ComboBox)
        .aria_label(current_label.clone())
        .relative()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .px(px(10.0))
        .py(px(4.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(line)
        .text_size(px(12.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ink)
        .cursor_pointer()
        .child(div().child(current_label))
        .child(div().flex_none().child(chevron))
        .child(
            canvas(move |bounds, _window, _cx| write_bounds.set(bounds), |_, _, _, _| {})
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _e, window, cx| {
                this.toggle_dropdown(
                    click_id.clone(),
                    trigger_bounds.get(),
                    items.clone(),
                    window,
                    cx,
                );
                cx.stop_propagation();
            }),
        )
        .into_any_element()
}

/// The menu-position + sizing knobs [`SettingsRootView::toggle_dropdown`] uses —
/// re-exported here so the geometry lives beside the trigger that produces it.
pub(crate) fn menu_position_for(trigger_bounds: Bounds<Pixels>) -> gpui::Point<Pixels> {
    // Bottom-RIGHT: the menu's top-right corner sits here (Corner::TopRight),
    // right-aligning the popup to its trigger like NSPopUpButton.
    trigger_bounds.bottom_right() + gpui::point(px(0.0), px(DROPDOWN_MENU_GAP))
}

/// The menu height cap (see [`DROPDOWN_MENU_MAX_HEIGHT`]).
pub(crate) fn menu_max_height() -> Pixels {
    px(DROPDOWN_MENU_MAX_HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::point;

    #[test]
    fn dropdown_item_carries_id_label_and_selection() {
        let item = DropdownItem::new("picker.opt", "Option", true, |_| {});
        assert_eq!(item.id.as_ref(), "picker.opt");
        assert_eq!(item.label.as_ref(), "Option");
        assert!(item.selected);
    }

    #[test]
    fn into_menu_item_preserves_id_label_and_checkmark() {
        let item = DropdownItem::new("picker.opt", "Option", true, |_| {});
        let row = item.into_menu_item();
        assert_eq!(row.entry_id(), Some("picker.opt"));
        assert_eq!(row.label(), Some("Option"));
        assert_eq!(row.selected(), Some(true));
        assert!(row.is_enabled());
    }

    #[test]
    fn slider_value_at_maps_linearly_and_clamps_to_the_track() {
        // Track from x=100, 170 wide, mapping to [55, 100] (the opacity range).
        let v = |x: f32| slider_value_at(x, 100.0, 170.0, 55.0, 100.0);
        assert_eq!(v(100.0), 55.0, "track left edge picks min");
        assert_eq!(v(270.0), 100.0, "track right edge picks max");
        assert_eq!(v(185.0), 77.5, "track midpoint picks the range midpoint");
        // Off-track positions clamp — a drag past either end pins the value.
        assert_eq!(v(0.0), 55.0);
        assert_eq!(v(1000.0), 100.0);
    }

    #[test]
    fn menu_anchors_under_the_trigger_bottom_right() {
        let bounds = Bounds {
            origin: point(px(100.0), px(50.0)),
            size: gpui::size(px(120.0), px(24.0)),
        };
        assert_eq!(
            menu_position_for(bounds),
            point(px(100.0 + 120.0), px(50.0 + 24.0 + DROPDOWN_MENU_GAP))
        );
    }
}
