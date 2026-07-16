//! The Font pane (R23 What-to-build item 4, The spec §Font, G9) — the live font
//! sliders + family picker + Reset, over the shared terminal
//! [`FontSettings`](nice_term_view::FontSettings) and the app-level
//! [`SharedSidebarFontSettings`](crate::settings::sidebar_font). Every control
//! applies LIVE (no apply button) and persists through the `fonts` section of
//! `ui_settings.json` (the [`SettingsPrefsStore`]).
//!
//! ## Stateless-pane shape
//! Like the Appearance / Claude panes this is a free `render` function: it reads
//! the shared font entities + the prefs store at build time and each control's
//! handler mutates them on `&mut App` (it takes the root view's `Context` only so
//! the family dropdown can host its popup — see
//! [`crate::settings::controls::dropdown`]). gpui has no native slider (D8), so
//! the two sizes are discrete steppers (−/readout/+) — a faithful-not-identical
//! functional port whose contract is "step → the exact `set_px` call + the exact
//! a11y id"; the family picker is an NSPopUpButton-style dropdown whose option
//! ids are the old chip ids.

use gpui::{
    div, prelude::*, px, AnyElement, App, Context, FontWeight, MouseButton, Rgba, SharedString,
    Window,
};

use nice_term_view::{clamp_line_height, DEFAULT_TERMINAL_FONT_PX, DEFAULT_TERMINAL_LINE_HEIGHT};

use crate::settings::controls::{dropdown, DropdownItem};
use crate::settings::prefs_store::SettingsPrefsStore;
use crate::settings::root::{setting_row, SettingsRootView};
use crate::settings::sidebar_font;
use crate::theme::slot_to_rgba;
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

/// Apply the terminal line-height multiplier LIVE (fan out through the shared
/// `FontSettings`, which re-metrics every grid) and persist the clamped value.
pub(crate) fn apply_terminal_line_height(cx: &mut App, multiplier: f32) {
    let Some(font) = crate::keymap::try_shared_font_settings(cx) else {
        return;
    };
    font.update(cx, |f, cx| f.set_line_height(multiplier, cx));
    let clamped = font.read(cx).line_height();
    persist(cx, |s| s.set_terminal_line_height(clamped));
    // Repaint the Settings window's own readout (the pane does not observe the
    // shared `FontSettings` — same discipline as `apply_terminal_px`).
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
    persist(cx, |s| {
        s.set_terminal_line_height(DEFAULT_TERMINAL_LINE_HEIGHT)
    });
    cx.refresh_windows();
}

// ===========================================================================
// Rendering
// ===========================================================================

/// The current terminal size + family-override (the family shown selected) +
/// line-height from the shared `FontSettings`, and the sidebar size.
fn current_state(cx: &App) -> (f32, Option<String>, f32, f32) {
    let (px, family, line_height) = match crate::keymap::try_shared_font_settings(cx) {
        Some(font) => {
            let f = font.read(cx);
            let chain = f.chain();
            // The default chain (3 entries) ⇒ "Default"; a single entry ⇒ an override.
            let family = if chain.len() == 1 {
                Some(chain[0].to_string())
            } else {
                None
            };
            (f.px(), family, f.line_height())
        }
        None => (DEFAULT_TERMINAL_FONT_PX, None, DEFAULT_TERMINAL_LINE_HEIGHT),
    };
    let sidebar_px = sidebar_font::current_sidebar_px(cx);
    (px, family, sidebar_px, line_height)
}

/// The Font pane body (The spec §Font).
pub(crate) fn font_pane(window: &mut Window, cx: &mut Context<SettingsRootView>) -> AnyElement {
    let slots = theme_settings::active_chrome_slots(cx);
    let ink = slot_to_rgba(slots.ink);
    let ink3 = slot_to_rgba(slots.ink3);
    let line = slot_to_rgba(slots.line);

    let (terminal_px, family, sidebar_px, line_height) = current_state(cx);

    let mut col = div().flex().flex_col().w_full().min_w(px(0.0));

    // --- App font family (drives the whole app: chrome + terminal) --------
    let installed = cx.text_system().all_font_names();
    let family_label: SharedString = match family.as_deref() {
        Some(fam) => SharedString::from(fam.to_string()),
        None => DEFAULT_FAMILY_LABEL.into(),
    };
    col = col.child(setting_row(
        "Font",
        dropdown(
            "settings.font.terminalFamily",
            family_label,
            family_dropdown_items(family_options(installed), family.as_deref()),
            window,
            cx,
        ),
        cx,
    ));

    // --- Terminal size ----------------------------------------------------
    col = col.child(setting_row(
        "Terminal size",
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

    // --- Terminal line height ---------------------------------------------
    col = col.child(setting_row(
        "Line height",
        line_height_stepper(
            "settings.font.lineHeight",
            line_height,
            ink,
            ink3,
            line,
            apply_terminal_line_height,
        ),
        cx,
    ));

    // --- Reset ------------------------------------------------------------
    col = col.child(setting_row("Reset", reset_button(ink, line), cx));

    col.into_any_element()
}

/// The "Default (SF Mono)" option's label (→ `apply_terminal_family(None)`).
const DEFAULT_FAMILY_LABEL: &str = "Default (SF Mono)";

/// The offered family list, in dropdown order: `(label, family-or-None)` — the
/// Default row (→ `None`), the curated candidates (in order), then every other
/// installed family alphabetized (deduped against the curated set). Pure so the
/// ordering/dedup contract is unit-testable without a text system.
fn family_options(installed: Vec<String>) -> Vec<(String, Option<String>)> {
    let curated: Vec<String> = CURATED_FAMILIES
        .iter()
        .filter(|c| installed.iter().any(|n| n == *c))
        .map(|s| s.to_string())
        .collect();

    let mut extra: Vec<String> = installed
        .into_iter()
        .filter(|n| !CURATED_FAMILIES.iter().any(|c| c == n))
        .collect();
    extra.sort();
    extra.dedup();

    let mut options = vec![(DEFAULT_FAMILY_LABEL.to_string(), None)];
    options.extend(
        curated
            .into_iter()
            .chain(extra)
            .map(|fam| (fam.clone(), Some(fam))),
    );
    options
}

/// The family dropdown options (option a11y ids
/// `settings.font.terminalFamily.default` / `settings.font.terminalFamily.{fam}`
/// — the old chip ids). Selection → [`apply_terminal_family`].
fn family_dropdown_items(
    options: Vec<(String, Option<String>)>,
    selected: Option<&str>,
) -> Vec<DropdownItem> {
    options
        .into_iter()
        .map(|(label, family)| {
            let id = match &family {
                Some(fam) => format!("settings.font.terminalFamily.{fam}"),
                None => "settings.font.terminalFamily.default".to_string(),
            };
            let is_selected = family.as_deref() == selected;
            let apply_family = family.map(SharedString::from);
            DropdownItem::new(id, label, is_selected, move |cx: &mut App| {
                apply_terminal_family(cx, apply_family.clone());
            })
        })
        .collect()
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

/// The line-height stepper step (matches the 0.05 slider granularity in the
/// plan). `−` / `<n.nn>` readout / `+`, each clamped by [`clamp_line_height`].
const LINE_HEIGHT_STEP: f32 = 0.05;

/// A discrete line-height stepper: `−` / `<n.nn>` readout / `+`, stepping by
/// [`LINE_HEIGHT_STEP`] and clamped to
/// `[MIN_TERMINAL_LINE_HEIGHT, MAX_TERMINAL_LINE_HEIGHT]`. `a11y` names the
/// container; the buttons are `<a11y>.dec` / `<a11y>.inc`.
fn line_height_stepper(
    a11y: &'static str,
    value: f32,
    ink: Rgba,
    ink3: Rgba,
    line: Rgba,
    apply: fn(&mut App, f32),
) -> impl IntoElement {
    let dec_id = SharedString::from(format!("{a11y}.dec"));
    let inc_id = SharedString::from(format!("{a11y}.inc"));
    let readout = SharedString::from(format!("{value:.2}"));
    // Snap to the step grid so repeated clicks land on clean values (e.g.
    // 1.30 → 1.35, not 1.2999…), then clamp to the allowed range.
    let snapped = (value / LINE_HEIGHT_STEP).round() * LINE_HEIGHT_STEP;
    let dec_value = clamp_line_height(snapped - LINE_HEIGHT_STEP);
    let inc_value = clamp_line_height(snapped + LINE_HEIGHT_STEP);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_options_lead_with_default_then_curated_then_alphabetized_extras() {
        // Two curated families, listed in `installed` OUT OF curated order
        // ("Menlo" before "SF Mono"), to prove the picker emits them in
        // CURATED_FAMILIES declared order — not in `installed` order — while
        // omitting uninstalled curated names ("Hack", "Cascadia Code", ...) to
        // assert those are dropped (availability-gated), not just the extras.
        let installed = vec![
            "Zapfino".to_string(),
            "Menlo".to_string(),   // curated (CURATED_FAMILIES index 3)
            "SF Mono".to_string(), // curated (CURATED_FAMILIES index 0)
            "Arial".to_string(),
            "Arial".to_string(),   // installed dupe — must dedup
        ];
        let options = family_options(installed);
        assert_eq!(options[0], (DEFAULT_FAMILY_LABEL.to_string(), None));

        // Installed curated families appear in CURATED_FAMILIES declared order
        // (SF Mono before Menlo), NOT the order they were listed in `installed`.
        assert_eq!(options[1].1.as_deref(), Some("SF Mono"));
        assert_eq!(options[2].1.as_deref(), Some("Menlo"));

        // Curated families absent from `installed` (e.g. "Hack") must not appear.
        assert!(
            !options
                .iter()
                .any(|(_, f)| f.as_deref() == Some("Hack"))
        );

        // Then the remaining (non-curated) installed families, alphabetized + deduped.
        let tail: Vec<&str> = options[3..]
            .iter()
            .map(|(_, f)| f.as_deref().unwrap())
            .collect();
        assert_eq!(tail, ["Arial", "Zapfino"]);
    }

    #[test]
    fn family_dropdown_items_keep_the_chip_ids_and_checkmark_the_selection() {
        let options = vec![
            (DEFAULT_FAMILY_LABEL.to_string(), None),
            ("Menlo".to_string(), Some("Menlo".to_string())),
        ];

        // No override ⇒ the Default row is the selection.
        let items = family_dropdown_items(options.clone(), None);
        assert_eq!(items[0].id.as_ref(), "settings.font.terminalFamily.default");
        assert_eq!(items[0].label.as_ref(), DEFAULT_FAMILY_LABEL);
        assert!(items[0].selected);
        assert_eq!(items[1].id.as_ref(), "settings.font.terminalFamily.Menlo");
        assert!(!items[1].selected);

        // A family override checkmarks exactly that row.
        let items = family_dropdown_items(options, Some("Menlo"));
        assert!(!items[0].selected);
        assert!(items[1].selected);
    }
}
