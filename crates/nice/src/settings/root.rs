//! The Settings window root view (R23 What-to-build items 2 + 8): the 160pt
//! section rail over a scrollable content area, the per-slug content dispatch, the
//! shared `SettingSubtitle` / `SettingRow` / `SettingTooltip` building blocks, and
//! the `shortcuts_pane` placeholder R24 fills.
//!
//! ## The pane-hosting seam (Exported contract — R24 fills the Shortcuts pane)
//! 1. [`settings_rail_sections`] is the fixed six-section rail (Editors CUT); it
//!    already contains `shortcuts` in position 2. R24 does NOT modify it.
//! 2. [`SettingsRootView::active`] (default `"appearance"`) is R23's selection
//!    state; the rail rows set it, the content area switches on it. R24 adds no
//!    selection wiring.
//! 3. [`render_section`] dispatches the active slug through the pure
//!    [`section_pane_for_slug`]; its `shortcuts` arm delegates to
//!    [`shortcuts_pane`], which slice 1 ships as a PLACEHOLDER. **R24's entire
//!    integration is to replace `shortcuts_pane`'s body** with its recorder-field
//!    ShortcutsPane, reading its own binding-store Global from `cx` and building
//!    its recorder capture from `window`. R24 touches this ONE function; it does
//!    not edit the rail, the selection state, or the dispatch match.
//!
//! The Appearance / Font / Claude / Advanced / About pane bodies are stubs in
//! slice 1 ([`pane_placeholder`]); later slices of this cycle replace them.

use std::rc::Rc;

use gpui::{
    div, prelude::*, px, AnyElement, App, Bounds, Context, DismissEvent, Entity, MouseButton,
    MouseMoveEvent, MouseUpEvent, Pixels, Render, SharedString, Subscription, Window,
};

use nice_theme::chrome_geometry::TOP_BAR_HEIGHT;
use nice_theme::color::Srgba;
use nice_theme::glass::{glass_fill, glass_line};
use nice_theme::palette::{ColorScheme, Slots};

use crate::context_menu::{ContextMenu, ContextMenuItem};
use crate::settings::controls::{self, DropdownItem};
use crate::theme::{slot_to_rgba, srgba_to_rgba};

/// Left-rail width (Swift `SettingsView.swift:113`).
const RAIL_WIDTH: f32 = 160.0;
/// Titlebar centered-title edge inset (per side). Matches the mock's `.win-title`
/// (`left: 90px; right: 90px`) and the main window's `SINGLE_TAB_EDGE_INSET` — so
/// the centered "Settings" label clears the native traffic-light cluster on the left.
const SETTINGS_TITLE_EDGE_INSET: f32 = 90.0;

/// The six settings sections in rail order — `(slug, rail label)`. This is the
/// Swift `SettingsSection` declaration order (`SettingsView.swift:31-58`) **minus
/// the CUT Editors pane** (roadmap §2). The `slug` is the stable a11y-id suffix
/// (`settings.section.<slug>`); the label is the user-visible rail text.
const RAIL_SECTIONS: &[(&str, &str)] = &[
    ("appearance", "Appearance"),
    ("shortcuts", "Shortcuts"),
    ("font", "Font"),
    ("claude", "Claude"),
    ("advanced", "Advanced"),
    ("about", "About"),
];

/// The ordered `(slug, rail label)` sections the rail renders (Exported contract).
/// R24 does NOT modify this — the `shortcuts` row already renders (a11y
/// `settings.section.shortcuts`).
pub(crate) fn settings_rail_sections() -> &'static [(&'static str, &'static str)] {
    RAIL_SECTIONS
}

/// The dispatch target for a rail slug — the pure routing decision behind
/// [`render_section`], split out so the slug→pane mapping is unit-testable without
/// a gpui window (the element builders need `&mut Window` / `&mut App`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SectionPane {
    Appearance,
    Shortcuts,
    Font,
    Claude,
    Advanced,
    About,
}

/// Route a rail slug to its pane. An unknown slug falls back to the default
/// section (`appearance`, `SettingsView.swift:68`) — the same fallback the content
/// area renders when `active` drifts.
pub(crate) fn section_pane_for_slug(slug: &str) -> SectionPane {
    match slug {
        "shortcuts" => SectionPane::Shortcuts,
        "font" => SectionPane::Font,
        "claude" => SectionPane::Claude,
        "advanced" => SectionPane::Advanced,
        "about" => SectionPane::About,
        // "appearance" and any unknown slug ⇒ the default section.
        _ => SectionPane::Appearance,
    }
}

/// The Settings window root: the 160pt section rail over a scrollable content
/// area. Holds the selected-section [`active`](Self::active) slug (default
/// `"appearance"`); the rail rows set it, the content switches on it.
pub(crate) struct SettingsRootView {
    /// The selected rail slug (default `"appearance"`). Owned by R23, untouched by
    /// R24 (Exported contract 2).
    active: SharedString,
    /// The open dropdown popup, if any — the panes stay stateless free functions,
    /// so the one open menu (trigger id + [`ContextMenu`] entity) lives here (the
    /// toolbar's `present_context_menu` owner pattern).
    open_dropdown: Option<OpenDropdown>,
    /// The in-flight slider drag, if any (armed by a [`controls::slider`]
    /// mouse-down; tracked by the root's mouse-move/up listeners — the
    /// sidebar-resize pattern, so the drag survives leaving the track).
    active_slider: Option<ActiveSliderDrag>,
}

/// A slider drag in flight: the track's window-space geometry, the value range,
/// and the live `apply` mutator to run at each pointer position.
struct ActiveSliderDrag {
    track_x: f32,
    track_w: f32,
    min: f32,
    max: f32,
    apply: Rc<dyn Fn(&mut App, f32)>,
}

/// The one open dropdown menu: which trigger opened it, the popup entity, and the
/// dismiss subscription keeping the state in sync.
struct OpenDropdown {
    trigger_id: SharedString,
    menu: Entity<ContextMenu>,
    _dismiss_sub: Subscription,
}

impl SettingsRootView {
    pub(crate) fn new() -> Self {
        Self {
            active: SharedString::from("appearance"),
            open_dropdown: None,
            active_slider: None,
        }
    }

    /// Arm a slider drag (a [`controls::slider`] track mouse-down): jump the
    /// value to the pressed `x` immediately (click-to-jump), then keep applying
    /// as the root's mouse-move listener tracks the pointer until release.
    pub(crate) fn begin_slider_drag(
        &mut self,
        track: Bounds<Pixels>,
        min: f32,
        max: f32,
        apply: Rc<dyn Fn(&mut App, f32)>,
        x: Pixels,
        cx: &mut Context<Self>,
    ) {
        let track_x = f32::from(track.origin.x);
        let track_w = f32::from(track.size.width);
        apply(cx, controls::slider_value_at(f32::from(x), track_x, track_w, min, max));
        self.active_slider = Some(ActiveSliderDrag { track_x, track_w, min, max, apply });
        cx.notify();
    }

    /// Root mouse-move: while a slider drag is armed and the button is still
    /// down, apply the value under the pointer; a move with the button released
    /// (missed mouse-up) just disarms.
    fn on_root_mouse_move(&mut self, e: &MouseMoveEvent, _w: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = &self.active_slider else {
            return;
        };
        if e.pressed_button == Some(MouseButton::Left) {
            let value = controls::slider_value_at(
                f32::from(e.position.x),
                drag.track_x,
                drag.track_w,
                drag.min,
                drag.max,
            );
            let apply = drag.apply.clone();
            apply(cx, value);
        } else {
            self.active_slider = None;
            cx.notify();
        }
    }

    /// Root mouse-up: the slider drag (if any) ends.
    fn on_root_mouse_up(&mut self, _e: &MouseUpEvent, _w: &mut Window, cx: &mut Context<Self>) {
        if self.active_slider.take().is_some() {
            cx.notify();
        }
    }

    /// Toggle a [`controls::dropdown`]'s menu: close it when this trigger's menu
    /// is already open, else open a [`ContextMenu`] of the options anchored under
    /// the trigger, at least as wide as it, checkmarking the selected option.
    /// Click-away / Esc / selection all emit [`DismissEvent`], which clears the
    /// state here.
    ///
    /// Ordering note: clicking the OPEN dropdown's own trigger both fires the
    /// menu's capture-phase click-away (`dismiss` → a DEFERRED [`DismissEvent`])
    /// and, in the bubble phase, this toggle. The bubble runs first, hits the
    /// same-trigger branch, and drops the menu + subscription — so the deferred
    /// event dies undelivered and the click reads as a clean close. The dismiss
    /// subscription guards on the menu entity so a stale dismissal can never
    /// clear a NEWER dropdown (e.g. opening B while A is open drops A first).
    pub(crate) fn toggle_dropdown(
        &mut self,
        trigger_id: SharedString,
        trigger_bounds: Bounds<Pixels>,
        items: Vec<DropdownItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(open) = self.open_dropdown.take() {
            cx.notify();
            if open.trigger_id == trigger_id {
                return;
            }
        }
        let menu_items: Vec<ContextMenuItem> =
            items.into_iter().map(DropdownItem::into_menu_item).collect();
        let menu = cx.new(|cx| {
            ContextMenu::new(controls::menu_position_for(trigger_bounds), menu_items, window, cx)
                .with_min_width(trigger_bounds.size.width)
                .with_max_height(controls::menu_max_height())
                .with_anchor_corner(gpui::Anchor::TopRight)
        });
        let sub = cx.subscribe_in(
            &menu,
            window,
            |this, menu, _ev: &DismissEvent, _window, cx| {
                if this
                    .open_dropdown
                    .as_ref()
                    .is_some_and(|open| open.menu == *menu)
                {
                    this.open_dropdown = None;
                }
                cx.notify();
            },
        );
        self.open_dropdown = Some(OpenDropdown {
            trigger_id,
            menu,
            _dismiss_sub: sub,
        });
        cx.notify();
    }

    /// One rail button: a11y id `settings.section.<slug>` + [`gpui::Role::Button`]
    /// + label. Restyle plan 06 flat nav (mock `.set-nav .row`): NO panel/selection
    /// fill — the active row is accent-colored text (semibold), inactive is medium
    /// `ink2`; the only fill is a faint `glass_fill` on hover. Mock geometry: 5×16
    /// padding, no rounded pill.
    fn rail_row(
        slug: &'static str,
        label: &'static str,
        is_active: bool,
        slots: Slots,
        accent: Srgba,
        scheme: ColorScheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let text_color = if is_active {
            srgba_to_rgba(accent)
        } else {
            slot_to_rgba(slots.ink2)
        };
        let weight = if is_active {
            gpui::FontWeight::SEMIBOLD
        } else {
            gpui::FontWeight::MEDIUM
        };
        let hover_fill = srgba_to_rgba(glass_fill(scheme));
        div()
            .id(SharedString::from(format!("settings.section.{slug}")))
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(label))
            .w_full()
            .px(px(16.0))
            .py(px(5.0))
            .text_size(px(12.5))
            .font_weight(weight)
            .text_color(text_color)
            .cursor_pointer()
            .hover(move |d| d.bg(hover_fill))
            .child(SharedString::from(label))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e, _window, cx| {
                    this.active = SharedString::from(slug);
                    cx.notify();
                }),
            )
    }
}

impl Render for SettingsRootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let slots = crate::theme_settings::active_chrome_slots(cx);
        let scheme = crate::theme_settings::active_chrome_scheme(cx);
        let accent = crate::theme_settings::active_chrome_accent(cx);
        // Restyle plan 06: the Settings window mirrors the main window's ONE
        // translucent surface — the active terminal theme's background at the
        // active-scheme opacity (identical to `app_shell::terminal_backing_color`,
        // the mock's `.window` background). The genuine NSWindow non-opacity + OS
        // blur are pushed separately (window.rs open + the transparency fanout), so
        // at opacity < 1.0 the desktop shows through this fill. No opaque panel
        // fills anywhere in the body.
        let (theme, _) = crate::theme_settings::active_terminal_theme_and_accent(cx);
        let opacity = crate::theme_settings::effective_window_opacity(window, cx);
        let surface = crate::app_shell::terminal_backing_color(&theme, opacity);
        // Over-glass hairline (scheme-scoped, not a palette slot) — the flat nav's
        // right edge, the same grammar as the flattened sidebar (plan 02).
        let hairline = srgba_to_rgba(glass_line(scheme));
        // Terminal-resolved mono family throughout (parity with the main window).
        let mono = crate::keymap::try_shared_font_settings(cx).map(|f| f.read(cx).family());

        // The 28pt titlebar: native traffic lights sit at their OS position (left);
        // the centered "Settings" label is inset both edges to clear them. Pure
        // painted text — no mouse listeners — so AppKit's titlebar drag passes
        // straight through (the window is `is_movable`).
        let titlebar = div()
            .flex_none()
            .w_full()
            .h(px(TOP_BAR_HEIGHT))
            .px(px(SETTINGS_TITLE_EDGE_INSET))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(slot_to_rgba(slots.ink2))
                    .child(SharedString::from("Settings")),
            );

        // Flat left nav (mock `.set-nav`): no card/panel fill, a `glass_line`
        // hairline at the right edge, one button per section.
        let mut rail = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(1.0))
            .w(px(RAIL_WIDTH))
            .py(px(8.0))
            .border_r_1()
            .border_color(hairline);
        for &(slug, label) in settings_rail_sections() {
            let is_active = self.active.as_ref() == slug;
            rail = rail.child(Self::rail_row(slug, label, is_active, slots, accent, scheme, cx));
        }

        // The scrollable content area (18/24 pad), dispatching per active slug. No
        // fill — the root's translucent surface shows through.
        let content = div()
            .id("settings.content")
            .flex_1()
            .min_w(px(0.0))
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    // Fill the scroll container's width (never its max-content):
                    // without this, one over-wide row widens the whole column and
                    // every control's shared right edge clips past the window.
                    .w_full()
                    .px(px(24.0))
                    .py(px(18.0))
                    .child(render_section(self.active.clone(), window, cx)),
            );

        // Rail + content below the titlebar; the body row fills the remaining height
        // (`min_h(0)` lets the scroll container size correctly).
        let body = div()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .flex_row()
            .child(rail)
            .child(content);

        div()
            .key_context("SettingsRoot")
            .size_full()
            .flex()
            .flex_col()
            .bg(surface)
            .when_some(mono, |d, fam| d.font_family(fam))
            // Slider-drag tracking (armed by a track mouse-down) — root-level so
            // the drag keeps applying when the pointer leaves the 170px track.
            .on_mouse_move(cx.listener(Self::on_root_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_root_mouse_up))
            .child(titlebar)
            .child(body)
            // The open dropdown menu (if any) — its own render defers + anchors
            // itself at window coords, so root-level placement is layout-neutral.
            .children(self.open_dropdown.as_ref().map(|open| open.menu.clone()))
    }
}

/// Build the content for the active section slug (the content-area dispatch). The
/// `shortcuts` arm delegates to [`shortcuts_pane`] (the R24 seam). Takes the root
/// view's `Context` because the Appearance / Font panes host dropdowns whose open
/// state lives on [`SettingsRootView`]; the other panes still read only globals
/// (their arms deref the `Context` down to `&mut App`).
pub(crate) fn render_section(
    active: SharedString,
    window: &mut Window,
    cx: &mut Context<SettingsRootView>,
) -> AnyElement {
    match section_pane_for_slug(active.as_ref()) {
        SectionPane::Appearance => appearance_pane(window, cx),
        SectionPane::Shortcuts => shortcuts_pane(window, cx),
        SectionPane::Font => font_pane(window, cx),
        SectionPane::Claude => claude_pane(window, cx),
        SectionPane::Advanced => advanced_pane(window, cx),
        SectionPane::About => about_pane(window, cx),
    }
}

// -- shared building blocks (mock `.set-row` / `.set-nav`, restyle plan 06) ----

/// A section header — a 14pt bold sub-header in primary ink with a hairline rule
/// below. Horizontal rules ONLY appear on section boundaries like this one (and
/// the Appearance pane's Light/Dark tab row); the per-row rules are gone.
pub(crate) fn setting_subtitle(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    let slots = crate::theme_settings::active_chrome_slots(cx);
    div()
        .w_full()
        .text_size(px(14.0))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(slot_to_rgba(slots.ink))
        .pt(px(24.0))
        .pb(px(6.0))
        .border_b_1()
        .border_color(slot_to_rgba(slots.line))
        .child(text.into())
}

/// The hover tooltip for a setting that keeps non-obvious info after the
/// description purge (mock feel-check: rows carry NO hint text). A themed
/// wrapping label box built through gpui's `div::tooltip` seam (the toolbar's
/// `TabTooltip` pattern).
pub(crate) struct SettingTooltip {
    pub(crate) text: SharedString,
}

impl Render for SettingTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let slots = crate::theme_settings::active_chrome_slots(cx);
        div()
            .px(px(10.0))
            .py(px(6.0))
            .rounded(px(6.0))
            .bg(slot_to_rgba(slots.panel))
            .border_1()
            .border_color(slot_to_rgba(slots.line))
            .max_w(px(320.0))
            .text_size(px(11.5))
            .text_color(slot_to_rgba(slots.ink))
            .child(self.text.clone())
    }
}

/// A `SettingRow` per the mock's `.set-row`: a 12px `ink2` label left, ONE
/// compact content-width control right-aligned on the shared right edge, 9pt
/// vertical padding, and NO bottom rule (rules mark sections, not rows). Reused
/// by every pane + the Shortcuts recorder rows.
pub(crate) fn setting_row(
    label: impl Into<SharedString>,
    control: impl IntoElement,
    cx: &App,
) -> impl IntoElement {
    setting_row_impl(label.into(), None, control, cx)
}

/// [`setting_row`] plus a small ⓘ info glyph after the label whose hover
/// tooltip carries the setting's non-obvious info (the only survivor of the
/// description purge — most rows have none).
pub(crate) fn setting_row_info(
    label: impl Into<SharedString>,
    info: impl Into<SharedString>,
    control: impl IntoElement,
    cx: &App,
) -> impl IntoElement {
    setting_row_impl(label.into(), Some(info.into()), control, cx)
}

fn setting_row_impl(
    label: SharedString,
    info: Option<SharedString>,
    control: impl IntoElement,
    cx: &App,
) -> impl IntoElement {
    let slots = crate::theme_settings::active_chrome_slots(cx);
    // `min_w(0)` lets the label column shrink below its text's natural width
    // instead of pushing the control past the right edge.
    let mut label_col = div()
        .flex()
        .flex_row()
        .items_center()
        .flex_1()
        .min_w(px(0.0))
        .gap(px(6.0))
        .child(
            div()
                .text_size(px(12.0))
                .text_color(slot_to_rgba(slots.ink2))
                .child(label.clone()),
        );
    if let Some(info) = info {
        // The ⓘ glyph is the tooltip's hover anchor (a11y
        // `settings.rowInfo.<label>`) — a plain text glyph in dimmed ink, no
        // click behavior.
        label_col = label_col.child(
            div()
                .id(SharedString::from(format!("settings.rowInfo.{label}")))
                .flex_none()
                .text_size(px(11.0))
                .text_color(slot_to_rgba(slots.ink3))
                .child("ⓘ")
                .tooltip(move |_window, cx| {
                    let text = info.clone();
                    cx.new(|_| SettingTooltip { text }).into()
                }),
        );
    }
    div()
        .flex()
        .flex_row()
        // Fill the pane column and never let the row's own min-content widen it
        // past the window (`min_w(0)` kills the floor).
        .w_full()
        .min_w(px(0.0))
        .items_center()
        .gap(px(16.0))
        .py(px(9.0))
        .child(label_col)
        .child(div().flex_none().child(control))
}

// -- panes --------------------------------------------------------------------

/// The Shortcuts pane body (Exported contract 3, R24 seam) — delegates to R24's
/// recorder-field [`ShortcutsPane`](crate::settings::shortcuts_pane), which reads
/// the [`ShortcutBindings`](crate::shortcuts_store::ShortcutBindings) Global from
/// `cx` and builds its capture from `window`. R24 replaced ONLY this body; the rail,
/// the selection state, the dispatch match, and the other panes are untouched.
pub(crate) fn shortcuts_pane(window: &mut Window, cx: &mut App) -> AnyElement {
    crate::settings::shortcuts_pane::shortcuts_pane(window, cx)
}

/// Appearance pane (The spec §Appearance) — full scope per D6.
fn appearance_pane(window: &mut Window, cx: &mut Context<SettingsRootView>) -> AnyElement {
    crate::settings::appearance_pane::appearance_pane(window, cx)
}

/// Font pane (The spec §Font, G9) — the live font sliders + family picker + Reset.
fn font_pane(window: &mut Window, cx: &mut Context<SettingsRootView>) -> AnyElement {
    crate::settings::font_pane::font_pane(window, cx)
}

/// Claude pane (The spec §Claude) — the single live theme-sync toggle.
fn claude_pane(window: &mut Window, cx: &mut App) -> AnyElement {
    crate::settings::claude_pane::claude_pane(window, cx)
}

/// Advanced pane (The spec §Advanced) — the persisted-inert smooth-scroll toggle.
fn advanced_pane(window: &mut Window, cx: &mut App) -> AnyElement {
    crate::settings::advanced_pane::advanced_pane(window, cx)
}

/// About pane (The spec §About) — the app name/version + tagline.
fn about_pane(window: &mut Window, cx: &mut App) -> AnyElement {
    crate::settings::about_pane::about_pane(window, cx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rail_is_the_six_slugs_minus_editors_in_order() {
        let slugs: Vec<&str> = settings_rail_sections()
            .iter()
            .map(|(slug, _)| *slug)
            .collect();
        assert_eq!(
            slugs,
            ["appearance", "shortcuts", "font", "claude", "advanced", "about"]
        );
        assert!(
            !slugs.contains(&"editors"),
            "the Editors pane is CUT (roadmap §2)"
        );
    }

    #[test]
    fn shortcuts_row_is_position_two() {
        // R24 slots its Shortcuts pane into position 2 (Exported contract 1).
        assert_eq!(settings_rail_sections()[1], ("shortcuts", "Shortcuts"));
    }

    #[test]
    fn each_rail_label_is_the_titlecased_slug() {
        for (slug, label) in settings_rail_sections() {
            assert_eq!(label.to_lowercase(), *slug);
        }
    }

    #[test]
    fn dispatch_routes_each_slug_including_the_shortcuts_seam() {
        use SectionPane::*;
        for (slug, _) in settings_rail_sections() {
            let expected = match *slug {
                "appearance" => Appearance,
                "shortcuts" => Shortcuts,
                "font" => Font,
                "claude" => Claude,
                "advanced" => Advanced,
                "about" => About,
                other => panic!("unexpected rail slug {other}"),
            };
            assert_eq!(section_pane_for_slug(slug), expected);
        }
        // The R24 seam: the `shortcuts` slug routes to its own placeholder arm.
        assert_eq!(section_pane_for_slug("shortcuts"), Shortcuts);
    }

    #[test]
    fn unknown_slug_falls_back_to_appearance() {
        // The content area renders the default section when `active` drifts.
        assert_eq!(section_pane_for_slug("nope"), SectionPane::Appearance);
        assert_eq!(section_pane_for_slug(""), SectionPane::Appearance);
    }

    #[test]
    fn default_active_section_is_appearance() {
        assert_eq!(SettingsRootView::new().active.as_ref(), "appearance");
    }
}
