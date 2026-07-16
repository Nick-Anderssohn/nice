//! The About pane (R23 What-to-build item 7, The spec §About) — two static lines:
//! the app name + version, and the tagline. The version comes from the bundle
//! `CFBundleShortVersionString` ([`crate::platform::main_bundle_short_version`]),
//! falling back to `CARGO_PKG_VERSION` for an unbundled `cargo run` / test binary.

use gpui::{div, prelude::*, px, AnyElement, App, FontWeight, SharedString, Window};

use crate::theme::slot_to_rgba;
use crate::theme_settings;

/// The displayed version: the bundle short version string, else the crate version.
pub(crate) fn about_version() -> String {
    crate::platform::main_bundle_short_version().unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

/// The About pane body (The spec §About).
pub(crate) fn about_pane(_window: &mut Window, cx: &mut App) -> AnyElement {
    let slots = theme_settings::active_chrome_slots(cx);
    let ink = slot_to_rgba(slots.ink);
    let ink2 = slot_to_rgba(slots.ink2);

    div()
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(0.0))
        .gap(px(4.0))
        .child(
            div()
                .text_size(px(13.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(ink)
                .child(SharedString::from(format!("Nice v{}", about_version()))),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(ink2)
                .child("A terminal emulator that auto-organizes claude instances."),
        )
        .into_any_element()
}
