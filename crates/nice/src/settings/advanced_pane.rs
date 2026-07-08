//! The Advanced pane (R23 What-to-build item 6, The spec §Advanced) — a single
//! "Smooth scrolling" toggle, **persisted but INERT** (Binding decision D2: the
//! scroll-animation backend is deferred `nice-term-*` feel work; the toggle
//! persists to `advanced.smooth_scroll` but has no rendering effect this cycle).

use gpui::{div, prelude::*, AnyElement, App, Window};

use crate::settings::controls::toggle_switch;
use crate::settings::prefs_store::SettingsPrefsStore;
use crate::settings::root::{setting_row, setting_title};

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

/// The Advanced pane body (The spec §Advanced). The "Smooth scrolling" control is
/// the shared [`toggle_switch`] (a11y `settings.advanced.smoothScrolling`);
/// click → [`toggle_smooth_scroll`] with the flipped value.
pub(crate) fn advanced_pane(_window: &mut Window, cx: &mut App) -> AnyElement {
    let on = smooth_scroll_on(cx);

    div()
        .flex()
        .flex_col()
        .child(setting_title("Advanced", cx))
        .child(setting_row(
            "Smooth scrolling",
            Some("Animates terminal scrolling.".into()),
            toggle_switch("settings.advanced.smoothScrolling", on, cx, move |cx| {
                toggle_smooth_scroll(cx, !on);
            }),
            cx,
        ))
        .into_any_element()
}
