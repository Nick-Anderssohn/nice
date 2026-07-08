//! The Settings window root view (R23 What-to-build items 2 + 8): the 160pt
//! section rail over a scrollable content area, the per-slug content dispatch, the
//! shared `SettingTitle` / `SettingSubtitle` / `SettingRow` building blocks, and
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

use gpui::{
    div, prelude::*, px, AnyElement, App, Bounds, Context, DismissEvent, Entity, MouseButton,
    Pixels, Render, SharedString, Subscription, Window,
};

use nice_theme::color::Srgba;
use nice_theme::palette::Slots;

use crate::context_menu::{ContextMenu, ContextMenuItem};
use crate::settings::controls::{self, DropdownItem};
use crate::theme::{slot_to_rgba, srgba_to_rgba, srgba_with_alpha};

/// Left-rail width (Swift `SettingsView.swift:113`).
const RAIL_WIDTH: f32 = 160.0;
/// Active rail-row background: the accent at this alpha (a faithful stand-in for
/// Swift's `niceSel(scheme, accent:)` selection tint).
const RAIL_ACTIVE_ALPHA: f32 = 0.18;

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
    /// + label; active row accent-selected + semibold ink, inactive medium ink2
    /// (Swift `SettingsSectionRow`, `SettingsView.swift:182-217`).
    fn rail_row(
        slug: &'static str,
        label: &'static str,
        is_active: bool,
        slots: Slots,
        accent: Srgba,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let text_color = if is_active {
            slot_to_rgba(slots.ink)
        } else {
            slot_to_rgba(slots.ink2)
        };
        let weight = if is_active {
            gpui::FontWeight::SEMIBOLD
        } else {
            gpui::FontWeight::MEDIUM
        };
        div()
            .id(SharedString::from(format!("settings.section.{slug}")))
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(label))
            .w_full()
            .px(px(10.0))
            .py(px(6.0))
            .rounded(px(6.0))
            .text_size(px(12.5))
            .font_weight(weight)
            .text_color(text_color)
            .cursor_pointer()
            .when(is_active, |d| {
                d.bg(srgba_to_rgba(srgba_with_alpha(accent, RAIL_ACTIVE_ALPHA)))
            })
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
        let accent = crate::theme_settings::active_chrome_accent(cx);
        let panel = slot_to_rgba(slots.panel);

        // 160pt left rail (Swift's floating-card treatment), one button per section.
        let mut rail = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(1.0))
            .w(px(RAIL_WIDTH))
            .p(px(6.0))
            .bg(slot_to_rgba(slots.background2))
            .border_r_1()
            .border_color(slot_to_rgba(slots.line));
        for &(slug, label) in settings_rail_sections() {
            let is_active = self.active.as_ref() == slug;
            rail = rail.child(Self::rail_row(slug, label, is_active, slots, accent, cx));
        }

        // The scrollable content area (18/24 pad), dispatching per active slug.
        let content = div()
            .id("settings.content")
            .flex_1()
            .overflow_y_scroll()
            .bg(panel)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .px(px(24.0))
                    .py(px(18.0))
                    .child(render_section(self.active.clone(), window, cx)),
            );

        div()
            .key_context("SettingsRoot")
            .size_full()
            .flex()
            .flex_row()
            .bg(panel)
            .child(rail)
            .child(content)
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

// -- shared building blocks (Swift SettingsView.swift:648-737) ----------------

/// `SettingTitle` (`SettingsView.swift:651-664`) — a 16pt bold pane heading in
/// primary ink with a 14pt bottom margin. Reused by every pane + R24's Shortcuts
/// pane.
pub(crate) fn setting_title(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    let slots = crate::theme_settings::active_chrome_slots(cx);
    div()
        .text_size(px(16.0))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(slot_to_rgba(slots.ink))
        .pb(px(14.0))
        .child(text.into())
}

/// `SettingSubtitle` (`SettingsView.swift:672-693`) — a 14pt bold subgroup header
/// in primary ink with extra top breathing room and a hairline rule below.
///
/// Exported for the Appearance / Font panes (slices 2/3) + R24's Shortcuts pane;
/// no in-crate caller until those land (the deliberately-exported-block pattern).
#[allow(dead_code)]
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

/// `SettingRow` (`SettingsView.swift:697-737`) — a flex label (with an optional
/// hint) on the left, ONE compact content-width control right-aligned on the
/// shared right edge (label/hint `flex_1`, control `flex_none`, vertically
/// centered), 10pt vertical padding and a 1pt `niceLine` bottom rule. Reused by
/// every pane + R24's recorder rows.
///
/// Exported for the panes (slices 2/3) + R24's Shortcuts pane; no in-crate caller
/// until those land (the deliberately-exported-block pattern).
#[allow(dead_code)]
pub(crate) fn setting_row(
    label: impl Into<SharedString>,
    hint: Option<SharedString>,
    control: impl IntoElement,
    cx: &App,
) -> impl IntoElement {
    let slots = crate::theme_settings::active_chrome_slots(cx);
    let mut label_col = div().flex().flex_col().flex_1().gap(px(2.0)).child(
        div()
            .text_size(px(13.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(slot_to_rgba(slots.ink))
            .child(label.into()),
    );
    if let Some(hint) = hint {
        label_col = label_col.child(
            div()
                .text_size(px(11.5))
                .text_color(slot_to_rgba(slots.ink3))
                .child(hint),
        );
    }
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .py(px(10.0))
        .border_b_1()
        .border_color(slot_to_rgba(slots.line))
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
