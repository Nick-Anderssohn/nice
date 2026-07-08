//! The Advanced pane (R23 What-to-build item 6, The spec §Advanced) — a single
//! "Smooth scrolling" toggle, **persisted but INERT** (Binding decision D2: the
//! scroll-animation backend is deferred `nice-term-*` feel work; the toggle
//! persists to `advanced.smooth_scroll` but has no rendering effect this cycle).

use gpui::{div, prelude::*, px, AnyElement, App, FontWeight, MouseButton, Rgba, SharedString, Window};

use crate::settings::prefs_store::SettingsPrefsStore;
use crate::settings::root::{setting_row, setting_title};
use crate::theme::{slot_to_rgba, srgba_to_rgba, srgba_with_alpha};
use crate::theme_settings;

/// The persisted smooth-scroll value (default OFF; absent store ⇒ OFF).
fn smooth_scroll_on(cx: &App) -> bool {
    cx.try_global::<SettingsPrefsStore>()
        .map(|s| s.smooth_scroll())
        .unwrap_or(false)
}

/// Persist the toggle (INERT — no live effect, D2). No-op when the store is absent.
pub(crate) fn toggle_smooth_scroll(cx: &mut App, on: bool) {
    if cx.try_global::<SettingsPrefsStore>().is_some() {
        let _ = cx.global_mut::<SettingsPrefsStore>().set_smooth_scroll(on);
    }
    cx.refresh_windows();
}

/// The Advanced pane body (The spec §Advanced).
pub(crate) fn advanced_pane(_window: &mut Window, cx: &mut App) -> AnyElement {
    let slots = theme_settings::active_chrome_slots(cx);
    let accent = theme_settings::active_chrome_accent(cx);
    let selected_bg = srgba_to_rgba(srgba_with_alpha(accent, 0.18));
    let ink = slot_to_rgba(slots.ink);
    let ink3 = slot_to_rgba(slots.ink3);

    let on = smooth_scroll_on(cx);

    div()
        .flex()
        .flex_col()
        .child(setting_title("Advanced", cx))
        .child(setting_row(
            "Smooth scrolling",
            Some("Animates terminal scrolling.".into()),
            smooth_toggle(on, selected_bg, ink, ink3),
            cx,
        ))
        .into_any_element()
}

/// The "Smooth scrolling" pill toggle (a11y `settings.advanced.smoothScrolling`).
fn smooth_toggle(on: bool, selected_bg: Rgba, ink: Rgba, ink3: Rgba) -> impl IntoElement {
    div()
        .id("settings.advanced.smoothScrolling")
        .role(gpui::Role::Button)
        .aria_label(if on { "On" } else { "Off" })
        .flex()
        .items_center()
        .justify_center()
        .w(px(52.0))
        .py(px(4.0))
        .rounded(px(6.0))
        .text_size(px(11.5))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .when(on, |d| d.bg(selected_bg).text_color(ink))
        .when(!on, |d| d.text_color(ink3))
        .child(SharedString::from(if on { "On" } else { "Off" }))
        .on_mouse_down(MouseButton::Left, move |_e, _window, cx: &mut App| {
            toggle_smooth_scroll(cx, !on);
        })
}
