//! The Font pane (R23 What-to-build item 4, The spec §Font, G9) — the live font
//! sliders + family picker + Reset, over the shared terminal
//! [`FontSettings`](nice_term_view::FontSettings) and the app-level
//! [`SharedSidebarFontSettings`](crate::settings::sidebar_font). Every control
//! applies LIVE (no apply button) and persists through the `fonts` section of
//! `ui_settings.json` (the [`SettingsPrefsStore`]).
//!
//! ## Stateless-pane shape
//! Like the Appearance / Claude panes this is a free `render` function
//! (`(&mut Window, &mut App) -> AnyElement`): it reads the shared font entities +
//! the prefs store at build time and each control's handler mutates them on
//! `&mut App`. gpui has no native slider (D8), so the two sizes are discrete
//! steppers (−/readout/+) — a faithful-not-identical functional port whose contract
//! is "step → the exact `set_px` call + the exact a11y id".

use gpui::{div, prelude::*, px, AnyElement, App, FontWeight, MouseButton, Rgba, SharedString, Window};

use nice_term_view::DEFAULT_TERMINAL_FONT_PX;

use crate::settings::prefs_store::SettingsPrefsStore;
use crate::settings::root::{setting_row, setting_title};
use crate::settings::sidebar_font;
use crate::theme::{slot_to_rgba, srgba_to_rgba, srgba_with_alpha};
use crate::theme_settings;

/// The curated monospace candidates (Swift `SettingsFontPane.swift:92-105`), mapped
/// to GPUI family names — offered in this order after the "Default (SF Mono)" row.
const CURATED_FAMILIES: &[&str] = &[
    "SF Mono",
    "JetBrains Mono NL",
    "JetBrains Mono",
    "Menlo",
    "Monaco",
    "Courier New",
    "PT Mono",
    "Fira Code",
    "Source Code Pro",
    "IBM Plex Mono",
    "Hack",
    "Cascadia Code",
];

// ===========================================================================
// Live-apply + persist handlers
// ===========================================================================

/// Persist a terminal-font mutation to the `fonts` section (no-op when the store
/// is absent — the isolated scenarios).
fn persist(cx: &mut App, apply: impl FnOnce(&mut SettingsPrefsStore) -> std::io::Result<bool>) {
    if cx.try_global::<SettingsPrefsStore>().is_some() {
        let _ = apply(cx.global_mut::<SettingsPrefsStore>());
    }
}

/// Apply a terminal font size LIVE (fan out through the shared `FontSettings`) and
/// persist the clamped value. The sidebar rescales proportionally via its own
/// `FontZoom` subscription (Swift parity).
pub(crate) fn apply_terminal_px(cx: &mut App, px: f32) {
    let Some(font) = crate::keymap::try_shared_font_settings(cx) else {
        return;
    };
    font.update(cx, |f, cx| f.set_px(px, cx));
    let clamped = font.read(cx).px();
    persist(cx, |s| s.set_terminal_font_px(clamped));
    // Repaint the Settings window's own controls (the `<n> pt` readout): the
    // `SettingsRootView` does NOT observe the shared `FontSettings`, so the
    // stateless pane only re-reads `current_state` on a window refresh. Matches
    // the sibling `apply_sidebar_px` / `reset_fonts` refresh discipline.
    cx.refresh_windows();
}

/// Apply a terminal font family LIVE (`None` ⇒ the default chain) and persist it.
pub(crate) fn apply_terminal_family(cx: &mut App, family: Option<SharedString>) {
    let Some(font) = crate::keymap::try_shared_font_settings(cx) else {
        return;
    };
    font.update(cx, |f, cx| f.set_family(family.clone(), cx));
    persist(cx, |s| {
        s.set_terminal_font_family(family.map(|f| f.to_string()))
    });
    // Repaint the Settings window so the selected-chip highlight updates (same
    // reason as `apply_terminal_px`: the pane does not observe `FontSettings`).
    cx.refresh_windows();
}

/// Apply a sidebar font size LIVE (rescales the sidebar chrome) and persist it.
pub(crate) fn apply_sidebar_px(cx: &mut App, px: f32) {
    let Some(sidebar) = sidebar_font::shared_sidebar_font(cx) else {
        return;
    };
    sidebar.update(cx, |s, cx| s.set_px(px, cx));
    let clamped = sidebar.read(cx).px();
    persist(cx, |s| s.set_sidebar_font_px(clamped));
    cx.refresh_windows();
}

/// Reset to shipped defaults: terminal → 13 + default chain, sidebar → 12
/// (`FontSettings.swift:102-105`). Both entities reset explicitly (the terminal's
/// `reset_to_defaults` deliberately does NOT emit `FontZoom`, so it does not fight
/// the explicit sidebar reset), and all three keys persist.
pub(crate) fn reset_fonts(cx: &mut App) {
    if let Some(font) = crate::keymap::try_shared_font_settings(cx) {
        font.update(cx, |f, cx| f.reset_to_defaults(cx));
    }
    if let Some(sidebar) = sidebar_font::shared_sidebar_font(cx) {
        sidebar.update(cx, |s, cx| s.reset(cx));
    }
    persist(cx, |s| s.set_terminal_font_px(DEFAULT_TERMINAL_FONT_PX));
    persist(cx, |s| s.set_terminal_font_family(None));
    persist(cx, |s| {
        s.set_sidebar_font_px(sidebar_font::DEFAULT_SIDEBAR_FONT_PX)
    });
    cx.refresh_windows();
}

// ===========================================================================
// Rendering
// ===========================================================================

/// The current terminal size + family-override (the family shown selected) from the
/// shared `FontSettings`, and the sidebar size.
fn current_state(cx: &App) -> (f32, Option<String>, f32) {
    let (px, family) = match crate::keymap::try_shared_font_settings(cx) {
        Some(font) => {
            let f = font.read(cx);
            let chain = f.chain();
            // The default chain (3 entries) ⇒ "Default"; a single entry ⇒ an override.
            let family = if chain.len() == 1 {
                Some(chain[0].to_string())
            } else {
                None
            };
            (f.px(), family)
        }
        None => (DEFAULT_TERMINAL_FONT_PX, None),
    };
    let sidebar_px = sidebar_font::current_sidebar_px(cx);
    (px, family, sidebar_px)
}

/// The Font pane body (The spec §Font).
pub(crate) fn font_pane(_window: &mut Window, cx: &mut App) -> AnyElement {
    let slots = theme_settings::active_chrome_slots(cx);
    let accent = theme_settings::active_chrome_accent(cx);
    let selected_bg = srgba_to_rgba(srgba_with_alpha(accent, 0.18));
    let selected_border = srgba_to_rgba(accent);
    let ink = slot_to_rgba(slots.ink);
    let ink3 = slot_to_rgba(slots.ink3);
    let line = slot_to_rgba(slots.line);

    let (terminal_px, family, sidebar_px) = current_state(cx);

    let mut col = div()
        .flex()
        .flex_col()
        .child(setting_title("Font", cx));

    // --- Terminal font family ---------------------------------------------
    col = col.child(setting_row(
        "Terminal font",
        Some(
            "Typeface for every terminal and Claude pane. Lists every font installed on \
             this Mac; monospace works best."
                .into(),
        ),
        family_control(cx, family.as_deref(), selected_bg, selected_border, ink, line),
        cx,
    ));

    // --- Terminal size ----------------------------------------------------
    col = col.child(setting_row(
        "Terminal size",
        Some("Monospace font size for every terminal and Claude pane.".into()),
        size_stepper(
            "settings.font.terminalSize",
            terminal_px,
            ink,
            ink3,
            line,
            apply_terminal_px,
        ),
        cx,
    ));

    // --- Sidebar size -----------------------------------------------------
    col = col.child(setting_row(
        "Sidebar size",
        Some("Base size for the sidebar. Other sidebar text scales proportionally.".into()),
        size_stepper(
            "settings.font.sidebarSize",
            sidebar_px,
            ink,
            ink3,
            line,
            apply_sidebar_px,
        ),
        cx,
    ));

    // --- Reset ------------------------------------------------------------
    col = col.child(setting_row(
        "Reset",
        None,
        reset_button(ink, line),
        cx,
    ));

    col.into_any_element()
}

/// The family picker: a "Default (SF Mono)" chip (→ `None`) then the curated
/// candidates then every other installed family, alphabetized. Selection →
/// [`apply_terminal_family`]. Container a11y `settings.font.terminalFamily`.
fn family_control(
    cx: &App,
    selected: Option<&str>,
    selected_bg: Rgba,
    selected_border: Rgba,
    ink: Rgba,
    line: Rgba,
) -> impl IntoElement {
    // The offered order: Default, curated (in order), then the remaining installed
    // families alphabetized (deduped against the curated set).
    let installed = cx.text_system().all_font_names();
    let mut extra: Vec<String> = installed
        .into_iter()
        .filter(|n| !CURATED_FAMILIES.iter().any(|c| c == n))
        .collect();
    extra.sort();
    extra.dedup();

    let mut row = div()
        .id("settings.font.terminalFamily")
        .flex()
        .flex_row()
        .flex_wrap()
        .gap(px(6.0));

    // "Default (SF Mono)" → None.
    let is_default = selected.is_none();
    row = row.child(family_chip(
        "settings.font.terminalFamily.default",
        "Default (SF Mono)",
        is_default,
        selected_bg,
        selected_border,
        ink,
        line,
        None,
    ));

    for fam in CURATED_FAMILIES.iter().map(|s| s.to_string()).chain(extra) {
        let is_sel = selected == Some(fam.as_str());
        row = row.child(family_chip(
            SharedString::from(format!("settings.font.terminalFamily.{fam}")),
            SharedString::from(fam.clone()),
            is_sel,
            selected_bg,
            selected_border,
            ink,
            line,
            Some(SharedString::from(fam)),
        ));
    }
    row
}

/// One family chip: click → [`apply_terminal_family`] with `family` (`None` = the
/// "Default" chip).
#[allow(clippy::too_many_arguments)]
fn family_chip(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    is_selected: bool,
    selected_bg: Rgba,
    selected_border: Rgba,
    ink: Rgba,
    line: Rgba,
    family: Option<SharedString>,
) -> impl IntoElement {
    div()
        .id(id.into())
        .role(gpui::Role::Button)
        .px(px(10.0))
        .py(px(4.0))
        .rounded(px(6.0))
        .border_1()
        .text_size(px(12.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ink)
        .cursor_pointer()
        .when(is_selected, |d| {
            d.bg(selected_bg).border_color(selected_border)
        })
        .when(!is_selected, |d| d.border_color(line))
        .child(label.into())
        .on_mouse_down(MouseButton::Left, move |_e, _window, cx: &mut App| {
            apply_terminal_family(cx, family.clone());
        })
}

/// A discrete size stepper: `−` / `<n> pt` readout / `+`, each clamped by the
/// underlying setter. `a11y` names the container; the buttons
/// are `<a11y>.dec` / `<a11y>.inc`. Click → `apply(cx, value ∓ 1)`.
fn size_stepper(
    a11y: &'static str,
    value: f32,
    ink: Rgba,
    ink3: Rgba,
    line: Rgba,
    apply: fn(&mut App, f32),
) -> impl IntoElement {
    let dec_id = SharedString::from(format!("{a11y}.dec"));
    let inc_id = SharedString::from(format!("{a11y}.inc"));
    let readout = SharedString::from(format!("{} pt", value.round() as i32));
    let dec_value = value - 1.0;
    let inc_value = value + 1.0;

    let button = |id: SharedString, glyph: &'static str, target: f32| {
        div()
            .id(id)
            .role(gpui::Role::Button)
            .flex()
            .items_center()
            .justify_center()
            .w(px(24.0))
            .py(px(3.0))
            .rounded(px(5.0))
            .border_1()
            .border_color(line)
            .text_size(px(13.0))
            .font_weight(FontWeight::MEDIUM)
            .text_color(ink)
            .cursor_pointer()
            .child(glyph)
            .on_mouse_down(MouseButton::Left, move |_e, _window, cx: &mut App| {
                apply(cx, target);
            })
    };

    div()
        .id(a11y)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .child(button(dec_id, "−", dec_value))
        .child(
            div()
                .w(px(44.0))
                .flex()
                .justify_center()
                .text_size(px(12.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(ink3)
                .child(readout),
        )
        .child(button(inc_id, "+", inc_value))
}

/// The "Reset to defaults" button (a11y `settings.font.reset`).
fn reset_button(ink: Rgba, line: Rgba) -> impl IntoElement {
    div()
        .id("settings.font.reset")
        .role(gpui::Role::Button)
        .aria_label("Reset to defaults")
        .px(px(12.0))
        .py(px(5.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(line)
        .text_size(px(12.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ink)
        .cursor_pointer()
        .child("Reset to defaults")
        .on_mouse_down(MouseButton::Left, move |_e, _window, cx: &mut App| {
            reset_fonts(cx);
        })
}
