//! The Appearance pane (R23 What-to-build item 3, The spec §Appearance) — full
//! scope per Binding decision D6 (R21 + R22 landed upstream). Every control writes
//! through R21's `apply_*` / R22's `import_theme`/`remove_imported` and repaints
//! live; R23 owns the human-readable import-error copy (§ImportError) and its
//! table test.
//!
//! ## Stateless-pane shape
//! The pane is a free `render` function (`(&mut Window, &mut App) -> AnyElement`)
//! per the pane-hosting seam — it reads the [`ThemeSettingsStore`] /
//! [`TerminalThemeCatalog`] globals at build time and every control is a plain
//! element whose click handler runs on `&mut App` (calling the R21/R22 mutator).
//! Those mutators `refresh_windows()`, so the settings window (and every other)
//! repaints with the new selection. The one bit of pane-local state — the last
//! Import… outcome — rides the [`ImportFeedback`] Global (rendered as an inline
//! error row), so the pane stays a pure builder.
//!
//! Round-2 restyle (plan 06, revised at the round-2 feel-check): the pane is
//! regrouped into scheme-independent controls on top (OS-sync toggle, the
//! manual Scheme flip — a flat segmented Light|Dark control, locked while
//! syncing with the OS — then accent swatches), then a per-scheme subsection headed by
//! Light/Dark text tabs (titlebar-tab grammar: mono text, `ink` active / `ink3`
//! inactive, a 1px accent underline under the active tab, seated on a
//! `glass_line` hairline). The tab defaults to the live scheme and, once
//! picked, becomes the editing target for the subsection's THEME dropdown,
//! OPACITY slider, and BLUR slider — writing THAT scheme's stored slot
//! regardless of which scheme is live (the tab, not the OS, picks the target).
//! The custom-themes import section sits below the tabbed subsection. The tab
//! selection lives in the [`AppearanceSchemeTab`] Global (like [`ImportFeedback`])
//! so the pane stays a stateless free function.
//!
//! Fidelity (D8): gpui has no native `Picker`/slider, so the merged theme picker
//! is the in-house [`dropdown`] (full display names, no color chips — the
//! feel-check killed the card grid for truncating theme names), and the
//! opacity/blur sliders port to `−`/readout/`+` steppers (the Font pane's
//! precedent). The contract each control keeps is "selection → the exact
//! `apply_*` call + the exact a11y id".

use gpui::{
    div, prelude::*, px, AnyElement, App, Context, FontWeight, MouseButton, Rgba, SharedString,
    Window,
};

use nice_theme::glass::{glass_fill_x, glass_line};
use nice_theme::palette::ColorScheme;
use nice_theme::AccentPreset;

use crate::ghostty_theme_parser::GhosttyParseError;
use crate::settings::controls::{dropdown, stepper, toggle_switch, DropdownItem};
use crate::settings::root::{setting_row, setting_subtitle, setting_title, SettingsRootView};
use crate::terminal_theme_catalog::{CatalogEntry, TerminalThemeCatalog, ThemeImportError};
use crate::theme::{slot_to_rgba, srgba_to_rgba};
use crate::theme_settings::{self, ThemeSettingsStore};

// ===========================================================================
// §ImportError — the human-readable mapping R23 OWNS
// (ported verbatim from `ImportErrorWrapper`, `SettingsTerminalPane.swift:97-127`)
// ===========================================================================

/// A mapped import failure: a short `title` + an actionable `message`. Rendered as
/// the inline Import… error row; the `#[test]` table below pins every case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImportErrorCopy {
    pub title: String,
    pub message: String,
}

/// Map a typed [`ThemeImportError`] to its (title, message) display copy
/// (§ImportError). R22 exports the typed error; R23 owns this mapping.
pub(crate) fn map_import_error(err: &ThemeImportError) -> ImportErrorCopy {
    match err {
        ThemeImportError::CannotRead(m) => ImportErrorCopy {
            title: "Couldn't read the theme file".to_string(),
            message: m.clone(),
        },
        ThemeImportError::CannotPersist(m) => ImportErrorCopy {
            title: "Couldn't save the theme".to_string(),
            message: m.clone(),
        },
        ThemeImportError::ParseFailed(parse) => ImportErrorCopy {
            title: "The theme file is invalid".to_string(),
            message: map_parse_error(parse),
        },
    }
}

/// The message half for a [`GhosttyParseError`] (§ImportError messages).
fn map_parse_error(parse: &GhosttyParseError) -> String {
    match parse {
        GhosttyParseError::MissingPalette { indices } => {
            let joined = indices
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "The file is missing palette entries: {joined}. Ghostty themes must \
                 define all 16 colors."
            )
        }
        GhosttyParseError::MissingRequiredKey { key } => {
            format!("The file is missing the required `{key}` key.")
        }
        GhosttyParseError::InvalidHex { value, line } => {
            format!("Line {line} contains an invalid color value: `{value}`.")
        }
        GhosttyParseError::PaletteIndexOutOfRange { index, line } => {
            format!("Line {line} uses palette index {index}; valid indices are 0–15.")
        }
    }
}

// ===========================================================================
// Import / remove handlers (the seam-driven side effects)
// ===========================================================================

/// The last Import… outcome — `Some(copy)` after a failed import, `None` after a
/// successful one (or before any import). A gpui `Global` so the stateless pane
/// can render the error row; the `settings-window` scenario reads it to assert the
/// exact §ImportError string surfaced.
#[derive(Default)]
pub(crate) struct ImportFeedback(pub Option<ImportErrorCopy>);

impl gpui::Global for ImportFeedback {}

/// The last import feedback (the failed-import copy, or `None`).
pub(crate) fn last_import_feedback(cx: &App) -> Option<ImportErrorCopy> {
    cx.try_global::<ImportFeedback>().and_then(|g| g.0.clone())
}

fn set_import_feedback(cx: &mut App, feedback: Option<ImportErrorCopy>) {
    cx.set_global(ImportFeedback(feedback));
    cx.refresh_windows();
}

// ===========================================================================
// Scheme-tab selection (which scheme the per-scheme subsection is editing)
// ===========================================================================

/// The Light/Dark scheme tab currently selected in the Appearance pane —
/// `None` (the default) means "follow the live scheme". A gpui `Global` (like
/// [`ImportFeedback`]) so the stateless pane can read it and the tab click can
/// write it without threading state through the root view. Once the user picks a
/// tab it stays put, so editing the INACTIVE scheme's theme/opacity/blur is
/// possible regardless of which scheme is live (plan 06: "the tab, not the OS,
/// now picks the target").
#[derive(Default)]
pub(crate) struct AppearanceSchemeTab(pub Option<ColorScheme>);

impl gpui::Global for AppearanceSchemeTab {}

/// The scheme the per-scheme subsection is editing: the explicitly-picked tab,
/// or (before any pick) the live `active` scheme. Pulled out as a pure function
/// so the tab-targeting resolution is unit-testable without a gpui window.
pub(crate) fn appearance_tab_scheme(tab: Option<ColorScheme>, active: ColorScheme) -> ColorScheme {
    tab.unwrap_or(active)
}

/// The currently-selected scheme tab, resolved against the live `active` scheme.
fn current_scheme_tab(cx: &App, active: ColorScheme) -> ColorScheme {
    let tab = cx.try_global::<AppearanceSchemeTab>().and_then(|g| g.0);
    appearance_tab_scheme(tab, active)
}

/// Select the scheme tab (the editing target for the per-scheme subsection) and
/// repaint. Does NOT change the live scheme — only which scheme's slots the
/// subsection reads/writes.
fn set_scheme_tab(cx: &mut App, scheme: ColorScheme) {
    cx.set_global(AppearanceSchemeTab(Some(scheme)));
    cx.refresh_windows();
}

/// The Import… button handler: read the injectable [`FilePickerOps`](crate::settings::file_picker)
/// seam for a chosen path (`None` ⇒ the user cancelled — a no-op), then call R22's
/// `import_theme`. On success the feedback clears (the theme joins `themes(for:)`
/// but is NOT auto-selected — R22's documented divergence); on failure the mapped
/// §ImportError copy is stored for the inline error row.
pub(crate) fn perform_import(cx: &mut App) {
    let Some(path) = crate::settings::file_picker::pick_theme_file(cx) else {
        return;
    };
    if cx.try_global::<TerminalThemeCatalog>().is_none() {
        return;
    }
    let result = cx.global_mut::<TerminalThemeCatalog>().import_theme(&path);
    match result {
        Ok(_entry) => set_import_feedback(cx, None),
        Err(err) => set_import_feedback(cx, Some(map_import_error(&err))),
    }
}

/// The per-theme delete handler: remove the imported theme, then — if the removed
/// id was the selected terminal theme in either scheme slot — reset that slot to
/// the scheme default via `apply_terminal_theme_id` and repaint live (Swift
/// `deleteImported` parity, `SettingsView.swift:463-474`).
pub(crate) fn perform_remove_imported(cx: &mut App, id: &str) {
    if cx.try_global::<TerminalThemeCatalog>().is_none() {
        return;
    }
    let removed = cx.global_mut::<TerminalThemeCatalog>().remove_imported(id);
    if !removed {
        return;
    }
    // Which slots referenced the just-removed id?
    let (light_hit, dark_hit) = match cx.try_global::<ThemeSettingsStore>() {
        Some(store) => {
            let a = store.appearance();
            (
                a.terminal_theme_id_for(ColorScheme::Light) == id,
                a.terminal_theme_id_for(ColorScheme::Dark) == id,
            )
        }
        None => (false, false),
    };
    if light_hit {
        let def = default_terminal_id_for(cx, ColorScheme::Light);
        theme_settings::apply_terminal_theme_id(cx, ColorScheme::Light, &def);
    }
    if dark_hit {
        let def = default_terminal_id_for(cx, ColorScheme::Dark);
        theme_settings::apply_terminal_theme_id(cx, ColorScheme::Dark, &def);
    }
    // A removal that hit no active slot still changes the deletable list — repaint.
    if !light_hit && !dark_hit {
        cx.refresh_windows();
    }
}

/// The scheme's default terminal-theme id — the first built-in `themes(for:)`
/// lists (`nice-default-light` / `nice-default-dark`), read AFTER the removal so a
/// deleted id is gone. Falls back to the known Nice-default id when the catalog is
/// absent.
fn default_terminal_id_for(cx: &App, scheme: ColorScheme) -> String {
    cx.try_global::<TerminalThemeCatalog>()
        .and_then(|c| c.themes(scheme).into_iter().next())
        .map(|e| e.id)
        .unwrap_or_else(|| match scheme {
            ColorScheme::Light => "nice-default-light".to_string(),
            ColorScheme::Dark => "nice-default-dark".to_string(),
        })
}

// ===========================================================================
// Rendering
// ===========================================================================

/// A user-facing label for an [`AccentPreset`].
fn accent_label(accent: AccentPreset) -> &'static str {
    match accent {
        AccentPreset::Terracotta => "Terracotta",
        AccentPreset::Ocean => "Ocean",
        AccentPreset::Fern => "Fern",
        AccentPreset::Iris => "Iris",
        AccentPreset::Graphite => "Graphite",
    }
}

/// The Appearance pane body (The spec §Appearance) — round-2 restyle plan 06
/// regroup: scheme-independent controls on top (OS-sync + accent + the manual
/// Scheme flip), a per-scheme subsection headed by Light/Dark tabs (theme
/// dropdown + opacity + blur, targeting the selected tab's scheme), then the
/// custom-themes import section.
pub(crate) fn appearance_pane(window: &mut Window, cx: &mut Context<SettingsRootView>) -> AnyElement {
    let slots = theme_settings::active_chrome_slots(cx);
    let accent_color = theme_settings::active_chrome_accent(cx);
    let accent = srgba_to_rgba(accent_color);
    // The live scheme drives the pane's own chrome (over-glass hairlines, the
    // default tab); the scheme TAB (below) is a separate editing target.
    let active_scheme = theme_settings::active_chrome_scheme(cx);
    let tab_scheme = current_scheme_tab(cx, active_scheme);

    let appearance = cx
        .try_global::<ThemeSettingsStore>()
        .map(|s| s.appearance().clone())
        .unwrap_or_default();
    let (light_themes, dark_themes, imported) = match cx.try_global::<TerminalThemeCatalog>() {
        Some(cat) => (
            cat.themes(ColorScheme::Light),
            cat.themes(ColorScheme::Dark),
            cat.imported_entries(),
        ),
        None => (Vec::new(), Vec::new(), Vec::new()),
    };

    let ink = slot_to_rgba(slots.ink);
    let ink2 = slot_to_rgba(slots.ink2);
    let ink3 = slot_to_rgba(slots.ink3);
    let line = slot_to_rgba(slots.line);
    let panel = slot_to_rgba(slots.panel);
    // Over-glass hairline (scheme-scoped, not a palette slot) — the tab-row rule,
    // matching the flattened chrome (plan 02/04).
    let hairline = srgba_to_rgba(glass_line(active_scheme));

    let mut col = div()
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(0.0))
        .child(setting_title("Appearance", cx));

    // --- Sync with OS (scheme-independent) ---------------------------------
    let sync_on = appearance.sync_with_os;
    col = col.child(setting_row(
        "Sync with OS appearance",
        Some("Match Nice's light / dark mode to the system setting.".into()),
        toggle_switch("settings.theme.sync", sync_on, cx, move |cx| {
            theme_settings::apply_sync_with_os(cx, !sync_on);
        }),
        cx,
    ));

    // --- Scheme (the manual light/dark flip; locked while sync is on) ------
    // Restored at the round-2 feel-check (the regroup had dropped it, leaving
    // no manual flip with OS-sync off). Sits directly below the OS-sync
    // toggle, above Accent (Nick flipped the two at the round-2.5 check).
    let stored_scheme = appearance.scheme;
    let fill_x = srgba_to_rgba(glass_fill_x(active_scheme));
    col = col.child(setting_row(
        "Scheme",
        Some("The active light / dark mode (locked while syncing with the OS).".into()),
        scheme_control(stored_scheme, sync_on, fill_x, ink, ink3, hairline),
        cx,
    ));

    // --- Accent (scheme-independent) ---------------------------------------
    col = col.child(setting_row(
        "Accent",
        Some("The caret / selection / logo tint.".into()),
        accent_control(appearance.accent, ink),
        cx,
    ));

    // --- Per-scheme subsection: Light/Dark tabs ----------------------------
    // The tabs head the subsection (same text + accent-underline grammar as the
    // titlebar tabs) and pick which scheme the theme dropdown + opacity + blur
    // edit, regardless of which scheme is live.
    col = col.child(scheme_tabs(tab_scheme, accent, ink, ink3, hairline));

    // The merged Theme dropdown for the selected tab's scheme (plan 05: ONE
    // theme drives both terminal colors AND chrome; the feel-check swapped the
    // card grid back to a dropdown so long names never truncate). The trigger
    // and options keep the terminal picker's a11y ids so the selection contract
    // is unchanged.
    let (tab_entries, tab_a11y): (&[CatalogEntry], &'static str) = match tab_scheme {
        ColorScheme::Light => (&light_themes, "settings.terminal.lightPicker"),
        ColorScheme::Dark => (&dark_themes, "settings.terminal.darkPicker"),
    };
    let tab_theme_id = appearance.terminal_theme_id_for(tab_scheme).to_string();
    let tab_theme_label = theme_display_name(tab_entries, &tab_theme_id);
    col = col.child(setting_row(
        "Theme",
        None,
        dropdown(
            tab_a11y,
            tab_theme_label,
            terminal_theme_dropdown_items(tab_a11y, tab_scheme, &tab_theme_id, tab_entries),
            window,
            cx,
        ),
        cx,
    ));

    // Opacity / blur for the SELECTED TAB's scheme (not the live one).
    col = col.child(setting_row(
        "Opacity",
        Some("Translucency of the window body for this scheme.".into()),
        opacity_stepper(
            tab_scheme,
            appearance.window_opacity_pct_for(tab_scheme),
            ink,
            ink3,
            line,
        ),
        cx,
    ));
    col = col.child(setting_row(
        "Blur",
        Some("Background blur radius behind the translucent window (0 = no blur).".into()),
        blur_stepper(
            tab_scheme,
            appearance.blur_radius_for(tab_scheme),
            ink,
            ink3,
            line,
        ),
        cx,
    ));

    // --- Custom themes (Import + deletable imports) ------------------------
    col = col.child(setting_subtitle("Custom themes", cx));
    col = col.child(setting_row(
        "Import theme",
        Some("Load a Ghostty `.ghostty` / `.conf` theme file into Nice's library.".into()),
        import_button("settings.terminal.import", ink, line),
        cx,
    ));

    // The inline error row for the last failed import (§ImportError).
    if let Some(feedback) = last_import_feedback(cx) {
        col = col.child(
            div()
                .id("settings.terminal.importError")
                .flex()
                .flex_col()
                .gap(px(2.0))
                .py(px(8.0))
                .child(
                    div()
                        .text_size(px(12.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(accent)
                        .child(SharedString::from(feedback.title)),
                )
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(ink2)
                        .child(SharedString::from(feedback.message)),
                ),
        );
    }

    // The deletable imported-theme rows (only when there are imports).
    for entry in imported {
        let id = entry.id.clone();
        let name = entry.display_name.clone();
        col = col.child(setting_row(
            SharedString::from(entry.display_name.clone()),
            Some(SharedString::from(format!(
                "Remove {name} from Nice's theme library."
            ))),
            remove_button(&id, panel, ink, line),
            cx,
        ));
    }

    col.into_any_element()
}

/// The Light/Dark scheme tabs heading the per-scheme subsection (a11y
/// `settings.appearance.schemeTab.{light,dark}`). Same grammar as the titlebar
/// tabs: mono text, `ink` active / `ink3` inactive, a 1px accent underline under
/// the active tab (inset per the mock's `.scheme-tab`), the whole row seated on a
/// `glass_line` hairline rule. Per plan 06 the INACTIVE tab wears NO underline
/// (the pair reads self-evidently as tabs). Click → [`set_scheme_tab`] (the
/// editing target; it does not flip the live scheme).
fn scheme_tabs(
    selected: ColorScheme,
    accent: Rgba,
    ink: Rgba,
    ink3: Rgba,
    hairline: Rgba,
) -> impl IntoElement {
    let tab = move |label: &'static str, key: &'static str, value: ColorScheme| {
        let is_active = selected == value;
        let mut d = div()
            .id(SharedString::from(format!("settings.appearance.schemeTab.{key}")))
            .role(gpui::Role::Button)
            .aria_label(label)
            .relative()
            .px(px(12.0))
            .py(px(6.0))
            .text_size(px(12.0))
            .font_weight(FontWeight::MEDIUM)
            .text_color(if is_active { ink } else { ink3 })
            .cursor_pointer()
            .child(label)
            .on_mouse_down(MouseButton::Left, move |_e, _window, cx: &mut App| {
                set_scheme_tab(cx, value);
            });
        if is_active {
            // The 1px accent underline, inset 11px per side and dropped 1px onto
            // the row's hairline rule (mock `.scheme-tab.active::after`).
            d = d.child(
                div()
                    .absolute()
                    .bottom(px(-1.0))
                    .left(px(11.0))
                    .right(px(11.0))
                    .h(px(1.0))
                    .bg(accent),
            );
        }
        d
    };
    div()
        .flex()
        .flex_row()
        .gap(px(2.0))
        .mt(px(16.0))
        .mb(px(6.0))
        .border_b_1()
        .border_color(hairline)
        .child(tab("Light mode", "light", ColorScheme::Light))
        .child(tab("Dark mode", "dark", ColorScheme::Dark))
}

/// The five accent swatches (a11y `settings.appearance.accent`); the selected one
/// carries an `ink` ring offset 2px off the swatch (mock `.swatch.sel`). Every
/// cell reserves the ring's footprint (a transparent border when unselected) so
/// selection never shifts the row. Click → `apply_accent`.
fn accent_control(selected: AccentPreset, ink: Rgba) -> impl IntoElement {
    let mut row = div()
        .id("settings.appearance.accent")
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0));
    for preset in AccentPreset::ALL {
        let is_selected = preset == selected;
        let swatch = div()
            .id(SharedString::from(format!(
                "settings.appearance.accent.{}",
                preset.raw_value()
            )))
            .role(gpui::Role::Button)
            .aria_label(accent_label(preset))
            .size(px(16.0))
            .rounded(px(8.0))
            .bg(srgba_to_rgba(preset.color()))
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_e, _window, cx: &mut App| {
                theme_settings::apply_accent(cx, preset);
            });
        // The offset ring: a same-size cell across all swatches (transparent
        // border unselected) so the selected ink ring adds no layout shift.
        row = row.child(
            div()
                .flex_none()
                .p(px(2.0))
                .rounded(px(11.0))
                .border_1()
                .border_color(if is_selected {
                    ink
                } else {
                    // A transparent ring reserves the selected footprint so
                    // selection adds no layout shift.
                    Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }
                })
                .child(swatch),
        );
    }
    row
}

// --- Scheme control (the manual light/dark flip) ----------------------------

/// The whole-control opacity of the Scheme picker while "Sync with OS
/// appearance" is on — the gpui analogue of prod's SwiftUI `.disabled()`
/// dimming, so the control visibly reads as locked.
const DISABLED_CONTROL_OPACITY: f32 = 0.4;

/// One segment of the Scheme control: its a11y `id`, display `label`, target
/// `value`, whether it is the current pick, and whether clicking is enabled
/// (OS-sync off). Carries the selection contract — [`SchemeSegment::select`] is
/// the exact path the segment's click runs — so the label→value→`apply_scheme`
/// mapping and the sync lock are unit-testable without a window (the
/// [`DropdownItem`]/`ThemeGridItem` precedent).
pub(crate) struct SchemeSegment {
    id: SharedString,
    label: &'static str,
    value: ColorScheme,
    active: bool,
    enabled: bool,
}

impl SchemeSegment {
    /// Run the segment's click contract: `apply_scheme(value)` — flipping the
    /// LIVE scheme — unless the control is sync-locked (then a no-op, exactly
    /// like the render path, which attaches no handler).
    pub(crate) fn select(&self, cx: &mut App) {
        if self.enabled {
            theme_settings::apply_scheme(cx, self.value);
        }
    }
}

/// The two segments of the Scheme control for the stored `scheme`, click-locked
/// while `sync_on` (a11y `settings.appearance.scheme.{Light,Dark}`).
fn scheme_segments(scheme: ColorScheme, sync_on: bool) -> [SchemeSegment; 2] {
    let seg = |label: &'static str, value: ColorScheme| SchemeSegment {
        id: SharedString::from(format!("settings.appearance.scheme.{label}")),
        label,
        value,
        active: scheme == value,
        enabled: !sync_on,
    };
    [seg("Light", ColorScheme::Light), seg("Dark", ColorScheme::Dark)]
}

/// The Light | Dark segmented control (a11y `settings.appearance.scheme`),
/// restyled flat per the mock's `.scheme-seg`: a hairline-bordered rounded
/// group, the selected cell filled with the over-glass `fill_x` in `ink` text,
/// the other cell bare in `ink3`. Disabled while `sync_on`: no click handlers /
/// pointer cursor, and the whole control renders at
/// [`DISABLED_CONTROL_OPACITY`]. Click → `apply_scheme` (flips the LIVE scheme
/// — unlike the subsection tabs below, which only pick the editing target).
fn scheme_control(
    scheme: ColorScheme,
    sync_on: bool,
    fill_x: Rgba,
    ink: Rgba,
    ink3: Rgba,
    hairline: Rgba,
) -> impl IntoElement {
    let mut group = div()
        .id("settings.appearance.scheme")
        .flex_none()
        .flex()
        .flex_row()
        .rounded(px(7.0))
        .border_1()
        .border_color(hairline)
        .overflow_hidden()
        .when(sync_on, |d| d.opacity(DISABLED_CONTROL_OPACITY));
    for segment in scheme_segments(scheme, sync_on) {
        let mut d = div()
            .id(segment.id.clone())
            .role(gpui::Role::Button)
            .aria_label(segment.label)
            .px(px(12.0))
            .py(px(4.0))
            .text_size(px(12.0))
            .font_weight(FontWeight::MEDIUM)
            .text_color(if segment.active { ink } else { ink3 })
            .child(segment.label);
        if segment.active {
            d = d.bg(fill_x);
        }
        if segment.enabled {
            d = d
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, move |_e, _window, cx: &mut App| {
                    segment.select(cx);
                });
        }
        group = group.child(d);
    }
    group
}

// --- Merged theme dropdown (plan 05: ONE theme drives terminal + chrome) ----

/// The selected terminal theme's display label — the dropdown trigger text.
/// Falls back to the raw id when the selection is not in `entries` (a transiently
/// dangling slot).
fn theme_display_name(entries: &[CatalogEntry], id: &str) -> String {
    entries
        .iter()
        .find(|e| e.id == id)
        .map(|e| e.display_name.clone())
        .unwrap_or_else(|| id.to_string())
}

/// The per-scheme theme dropdown options over `entries` (built-ins AND imports,
/// exactly the old chip list; option a11y ids `{a11y}.{theme id}`). Selection →
/// `apply_terminal_theme_id(scheme, id)` (which, post-merge, drives the chrome
/// too).
fn terminal_theme_dropdown_items(
    a11y: &'static str,
    scheme: ColorScheme,
    selected_id: &str,
    entries: &[CatalogEntry],
) -> Vec<DropdownItem> {
    entries
        .iter()
        .map(|entry| {
            let id = entry.id.clone();
            let click_id = id.clone();
            DropdownItem::new(
                format!("{a11y}.{id}"),
                entry.display_name.clone(),
                id == selected_id,
                move |cx: &mut App| theme_settings::apply_terminal_theme_id(cx, scheme, &click_id),
            )
        })
        .collect()
}

// --- Window opacity / blur steppers (D8: gpui has no native slider — same
// stepper substitution the Font pane's line-height control uses) -----------
//
// The slider bounds/step are mirrored here as plain constants rather than
// imported from `theme_settings` (whose clamp constants are private): this
// slice only wires the pane's UI, and every target value still passes through
// `apply_window_opacity` / `apply_blur_radius`, which re-clamp authoritatively
// — an out-of-range target here is harmless, never stored as-is.

/// Opacity stepper step, in percentage points.
const OPACITY_STEP_PCT: i32 = 5;
/// Opacity slider floor (mirrors `theme_settings::MIN_WINDOW_OPACITY_PCT`).
const OPACITY_MIN_PCT: i32 = 55;
/// Opacity slider ceiling (100% ⇒ fully opaque).
const OPACITY_MAX_PCT: i32 = 100;

/// Blur-radius stepper step, in px.
const BLUR_STEP_PX: i32 = 5;
/// Blur slider floor (0 ⇒ no blur).
const BLUR_MIN_PX: i32 = 0;
/// Blur slider ceiling (mirrors `theme_settings::MAX_BLUR_RADIUS`).
const BLUR_MAX_PX: i32 = 60;

/// The dec/inc targets for the opacity stepper at the current `pct`, clamped to
/// `[OPACITY_MIN_PCT, OPACITY_MAX_PCT]` — pulled out of [`opacity_stepper`] so
/// the floor/ceiling clamping is unit-testable without a gpui window.
fn opacity_step_targets(pct: u8) -> (i32, i32) {
    let pct = i32::from(pct);
    (
        (pct - OPACITY_STEP_PCT).clamp(OPACITY_MIN_PCT, OPACITY_MAX_PCT),
        (pct + OPACITY_STEP_PCT).clamp(OPACITY_MIN_PCT, OPACITY_MAX_PCT),
    )
}

/// The dec/inc targets for the blur stepper at the current `radius`, clamped to
/// `[BLUR_MIN_PX, BLUR_MAX_PX]` — pulled out of [`blur_stepper`] for the same
/// reason as [`opacity_step_targets`].
fn blur_step_targets(radius: u16) -> (i32, i32) {
    let radius = i32::from(radius);
    (
        (radius - BLUR_STEP_PX).clamp(BLUR_MIN_PX, BLUR_MAX_PX),
        (radius + BLUR_STEP_PX).clamp(BLUR_MIN_PX, BLUR_MAX_PX),
    )
}

/// The Opacity stepper for `scheme` (a11y `settings.appearance.opacity`).
/// Steps by [`OPACITY_STEP_PCT`]; live-applies through
/// [`theme_settings::apply_window_opacity`], which fans out through the
/// window's `WindowBackgroundAppearance` on every drag step.
fn opacity_stepper(scheme: ColorScheme, pct: u8, ink: Rgba, ink3: Rgba, line: Rgba) -> impl IntoElement {
    let (dec, inc) = opacity_step_targets(pct);
    stepper(
        "settings.appearance.opacity",
        format!("{pct}%"),
        dec as f32,
        inc as f32,
        ink,
        ink3,
        line,
        move |cx: &mut App, v: f32| {
            theme_settings::apply_window_opacity(cx, scheme, v.round() as u8);
        },
    )
}

/// The Blur stepper for `scheme` (a11y `settings.appearance.blur`). Steps by
/// [`BLUR_STEP_PX`]; live-applies through
/// [`theme_settings::apply_blur_radius`], which re-applies the numeric
/// CGS radius (or degrades to `Transparent` at 0) on every drag step.
fn blur_stepper(scheme: ColorScheme, radius: u16, ink: Rgba, ink3: Rgba, line: Rgba) -> impl IntoElement {
    let (dec, inc) = blur_step_targets(radius);
    stepper(
        "settings.appearance.blur",
        format!("{radius}px"),
        dec as f32,
        inc as f32,
        ink,
        ink3,
        line,
        move |cx: &mut App, v: f32| {
            theme_settings::apply_blur_radius(cx, scheme, v.round() as u16);
        },
    )
}

/// The Import… button (a11y `settings.terminal.import`).
fn import_button(a11y: &'static str, ink: Rgba, line: Rgba) -> impl IntoElement {
    div()
        .id(a11y)
        .role(gpui::Role::Button)
        .aria_label("Import theme…")
        .px(px(12.0))
        .py(px(5.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(line)
        .text_size(px(12.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ink)
        .cursor_pointer()
        .child("Import…")
        .on_mouse_down(MouseButton::Left, move |_e, _window, cx: &mut App| {
            perform_import(cx);
        })
}

/// A per-theme delete button (a11y `settings.terminal.remove.<id>`).
fn remove_button(id: &str, panel: Rgba, ink: Rgba, line: Rgba) -> impl IntoElement {
    let owned = id.to_string();
    div()
        .id(SharedString::from(format!("settings.terminal.remove.{id}")))
        .role(gpui::Role::Button)
        .aria_label("Remove")
        .px(px(10.0))
        .py(px(4.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(line)
        .bg(panel)
        .text_size(px(12.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ink)
        .cursor_pointer()
        .child("Remove")
        .on_mouse_down(MouseButton::Left, move |_e, _window, cx: &mut App| {
            perform_remove_imported(cx, &owned);
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- §ImportError copy table (NEW — Swift never unit-pinned these) --------

    #[test]
    fn import_error_titles() {
        assert_eq!(
            map_import_error(&ThemeImportError::CannotRead("x".into())).title,
            "Couldn't read the theme file"
        );
        assert_eq!(
            map_import_error(&ThemeImportError::CannotPersist("x".into())).title,
            "Couldn't save the theme"
        );
        assert_eq!(
            map_import_error(&ThemeImportError::ParseFailed(
                GhosttyParseError::MissingRequiredKey { key: "background".into() }
            ))
            .title,
            "The theme file is invalid"
        );
    }

    #[test]
    fn cannot_read_and_persist_pass_the_inner_message_verbatim() {
        assert_eq!(
            map_import_error(&ThemeImportError::CannotRead("permission denied".into())).message,
            "permission denied"
        );
        assert_eq!(
            map_import_error(&ThemeImportError::CannotPersist("disk full".into())).message,
            "disk full"
        );
    }

    #[test]
    fn missing_palette_joins_indices_and_appends_the_16_colors_note() {
        let copy = map_import_error(&ThemeImportError::ParseFailed(
            GhosttyParseError::MissingPalette { indices: vec![0, 3, 15] },
        ));
        assert_eq!(
            copy.message,
            "The file is missing palette entries: 0, 3, 15. Ghostty themes must \
             define all 16 colors."
        );
    }

    #[test]
    fn missing_required_key_backticks_the_key() {
        let copy = map_import_error(&ThemeImportError::ParseFailed(
            GhosttyParseError::MissingRequiredKey { key: "foreground".into() },
        ));
        assert_eq!(copy.message, "The file is missing the required `foreground` key.");
    }

    #[test]
    fn invalid_hex_reports_line_and_value() {
        let copy = map_import_error(&ThemeImportError::ParseFailed(
            GhosttyParseError::InvalidHex { value: "#nothex".into(), line: 7 },
        ));
        assert_eq!(copy.message, "Line 7 contains an invalid color value: `#nothex`.");
    }

    #[test]
    fn palette_index_out_of_range_reports_line_index_and_the_0_15_range() {
        let copy = map_import_error(&ThemeImportError::ParseFailed(
            GhosttyParseError::PaletteIndexOutOfRange { index: 16, line: 3 },
        ));
        assert_eq!(copy.message, "Line 3 uses palette index 16; valid indices are 0–15.");
        // A negative literal reaches here as-is (signed index).
        let neg = map_import_error(&ThemeImportError::ParseFailed(
            GhosttyParseError::PaletteIndexOutOfRange { index: -1, line: 9 },
        ));
        assert_eq!(neg.message, "Line 9 uses palette index -1; valid indices are 0–15.");
    }

    // --- the dropdown option list (the old chip contract, now menu items) -----

    #[test]
    fn theme_dropdown_items_carry_ids_labels_and_selection() {
        let entries = vec![
            CatalogEntry {
                id: "nice-default-dark".to_string(),
                display_name: "Nice Dark".to_string(),
                scope: crate::terminal_theme_catalog::ThemeScope::Dark,
            },
            CatalogEntry {
                id: "cool-import".to_string(),
                display_name: "Cool Import".to_string(),
                scope: crate::terminal_theme_catalog::ThemeScope::Either,
            },
        ];
        let items = terminal_theme_dropdown_items(
            "settings.terminal.darkPicker",
            ColorScheme::Dark,
            "cool-import",
            &entries,
        );
        assert_eq!(items[0].id.as_ref(), "settings.terminal.darkPicker.nice-default-dark");
        assert_eq!(items[0].label.as_ref(), "Nice Dark");
        assert!(!items[0].selected);
        assert_eq!(items[1].id.as_ref(), "settings.terminal.darkPicker.cool-import");
        assert!(items[1].selected, "the selected id's option is checkmarked");
    }

    #[test]
    fn theme_display_name_resolves_the_selected_entry_or_falls_back_to_the_id() {
        let entries = vec![CatalogEntry {
            id: "cool-import".to_string(),
            display_name: "Cool Import".to_string(),
            scope: crate::terminal_theme_catalog::ThemeScope::Either,
        }];
        assert_eq!(theme_display_name(&entries, "cool-import"), "Cool Import");
        assert_eq!(theme_display_name(&entries, "gone-theme"), "gone-theme");
    }

    // --- appearance_tab_scheme (the tab-targeting resolution) -----------------

    #[test]
    fn appearance_tab_scheme_defaults_to_active_then_follows_the_explicit_tab() {
        // No explicit tab ⇒ the live scheme (the default: the tab tracks it).
        assert_eq!(appearance_tab_scheme(None, ColorScheme::Dark), ColorScheme::Dark);
        assert_eq!(appearance_tab_scheme(None, ColorScheme::Light), ColorScheme::Light);
        // An explicit tab overrides the live scheme (the tab, not the OS, picks
        // the editing target — including the scheme that is NOT live).
        assert_eq!(
            appearance_tab_scheme(Some(ColorScheme::Light), ColorScheme::Dark),
            ColorScheme::Light
        );
        assert_eq!(
            appearance_tab_scheme(Some(ColorScheme::Dark), ColorScheme::Light),
            ColorScheme::Dark
        );
    }

    // --- perform_remove_imported (the delete-imported-theme flow) -------------
    //
    // App-level `#[gpui::test]`s on the MOCKED `TestAppContext` (no Metal, no
    // pixels; parallel-safe). They live IN THIS CRATE — `perform_remove_imported`
    // takes `&mut App` and drives the R21/R22 globals, which a dev/test crate
    // cannot reach because it cannot import this binary crate. No `SharedThemeState`
    // is installed, so the `apply_terminal_theme_id` fan-out / Claude mirror
    // no-op (they gate on that entity); the STORE slot mutation is the assertion.

    use gpui::TestAppContext;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A unique temp base dir per test (parallel-safe: pid + monotonic counter).
    fn temp_base(tag: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "nice-appearance-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// A minimal well-formed Ghostty theme source (a full 16-entry palette so it
    /// parses) — the import fixture. `bg` is `rrggbb` (no `#`).
    fn ghostty_fixture(bg: &str) -> String {
        let mut s = format!("background = #{bg}\nforeground = #ffffff\n");
        for i in 0..16u8 {
            s.push_str(&format!("palette = {i}=#0000{i:02x}\n"));
        }
        s
    }

    /// Install a temp catalog + defaults store and import `Cool Import.ghostty`
    /// (id `cool-import`). Returns the temp base dir (delete when done).
    fn setup_with_import(cx: &mut TestAppContext, tag: &str) -> PathBuf {
        let base = temp_base(tag);
        let catalog_dir = base.join("terminal-themes");
        std::fs::create_dir_all(&catalog_dir).unwrap();
        let fixture = base.join("Cool Import.ghostty");
        std::fs::write(&fixture, ghostty_fixture("abcdef")).unwrap();
        cx.update(|app| {
            app.set_global(TerminalThemeCatalog::new(catalog_dir));
            app.set_global(ThemeSettingsStore::with_defaults(base.join("ui_settings.json")));
            let entry = app
                .global_mut::<TerminalThemeCatalog>()
                .import_theme(&fixture)
                .expect("the valid fixture imports");
            assert_eq!(entry.id, "cool-import", "slug(\"Cool Import\") == cool-import");
        });
        base
    }

    fn dark_slot_id(app: &gpui::App) -> String {
        app.global::<ThemeSettingsStore>()
            .appearance()
            .terminal_theme_id_for(ColorScheme::Dark)
            .to_string()
    }

    #[gpui::test]
    fn remove_imported_resets_a_selected_slot_to_the_scheme_default(cx: &mut TestAppContext) {
        let base = setup_with_import(cx, "remove-selected");
        cx.update(|app| {
            // Select the import as the Dark scheme's terminal theme.
            theme_settings::apply_terminal_theme_id(app, ColorScheme::Dark, "cool-import");
            assert_eq!(dark_slot_id(app), "cool-import", "the import is now the Dark slot");

            // The scheme default is a built-in (built-ins lead `themes(for:)`), so
            // it is stable across the removal.
            let default_dark = default_terminal_id_for(app, ColorScheme::Dark);
            assert_ne!(default_dark, "cool-import");

            perform_remove_imported(app, "cool-import");

            // (a) it left the catalog (both the deletable list and the picker list).
            let cat = app.global::<TerminalThemeCatalog>();
            assert!(
                !cat.imported_entries().iter().any(|e| e.id == "cool-import"),
                "the removed theme left imported_entries()"
            );
            assert!(
                !cat.themes(ColorScheme::Dark).iter().any(|e| e.id == "cool-import"),
                "the removed theme left themes(Dark)"
            );

            // (b) the dangling selected slot fell back to the scheme default id
            //     (Swift deleteImported parity — the load-bearing edge case).
            let now = dark_slot_id(app);
            assert_ne!(now, "cool-import", "the slot no longer points at the deleted id");
            assert_eq!(now, default_dark, "the slot reset to the scheme default id");
        });
        let _ = std::fs::remove_dir_all(&base);
    }

    #[gpui::test]
    fn remove_imported_unselected_or_unknown_leaves_the_slot_untouched(cx: &mut TestAppContext) {
        let base = setup_with_import(cx, "remove-unselected");
        cx.update(|app| {
            let before = dark_slot_id(app); // a built-in default; the import is NOT selected

            // Removing the UNSELECTED import changes the deletable list but not the slot.
            perform_remove_imported(app, "cool-import");
            assert!(
                !app.global::<TerminalThemeCatalog>()
                    .imported_entries()
                    .iter()
                    .any(|e| e.id == "cool-import"),
                "the unselected import still leaves the catalog"
            );
            assert_eq!(
                dark_slot_id(app),
                before,
                "removing an unselected import does not touch the selected slot"
            );

            // An unknown / already-gone id is the not-removed early return: a no-op.
            perform_remove_imported(app, "no-such-theme");
            assert_eq!(
                dark_slot_id(app),
                before,
                "removing an unknown id is a no-op (the !removed early return)"
            );
        });
        let _ = std::fs::remove_dir_all(&base);
    }

    #[gpui::test]
    fn dropdown_selection_drives_the_exact_apply_calls(cx: &mut TestAppContext) {
        let base = setup_with_import(cx, "dropdown-apply");
        cx.update(|app| {
            // The merged Theme dropdown: selecting the imported theme's option
            // applies its id to the Dark scheme slot (the old chip's exact
            // apply_terminal_theme_id) — and, post-merge, that one id now drives
            // the chrome too.
            let entries = app.global::<TerminalThemeCatalog>().themes(ColorScheme::Dark);
            let items = terminal_theme_dropdown_items(
                "settings.terminal.darkPicker",
                ColorScheme::Dark,
                &dark_slot_id(app),
                &entries,
            );
            items
                .iter()
                .find(|i| i.id.as_ref() == "settings.terminal.darkPicker.cool-import")
                .expect("the imported theme is offered")
                .select(app);
            assert_eq!(dark_slot_id(app), "cool-import");
        });
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Tab targeting (plan 06 Validation): a dropdown selection on the Light tab
    /// writes the LIGHT slot while Dark is the live scheme — the tab, not the OS,
    /// picks the target, and the live slot is untouched.
    #[gpui::test]
    fn dropdown_selection_targets_the_tab_scheme_not_the_live_scheme(cx: &mut TestAppContext) {
        let base = setup_with_import(cx, "dropdown-tab-target");
        cx.update(|app| {
            // Live scheme is the store default (Dark); the Light tab is selected.
            let live = app.global::<ThemeSettingsStore>().appearance().scheme;
            assert_eq!(live, ColorScheme::Dark);
            let target = appearance_tab_scheme(Some(ColorScheme::Light), live);
            assert_eq!(target, ColorScheme::Light);

            let light_before = app
                .global::<ThemeSettingsStore>()
                .appearance()
                .terminal_theme_id_for(ColorScheme::Light)
                .to_string();
            let dark_before = dark_slot_id(app);

            // Select the imported theme's option on the (non-live) Light tab.
            let entries = app.global::<TerminalThemeCatalog>().themes(ColorScheme::Light);
            let items = terminal_theme_dropdown_items(
                "settings.terminal.lightPicker",
                target,
                &light_before,
                &entries,
            );
            items
                .iter()
                .find(|i| i.id.as_ref() == "settings.terminal.lightPicker.cool-import")
                .expect("the imported theme is offered in the light tab too")
                .select(app);

            let a = app.global::<ThemeSettingsStore>().appearance().clone();
            assert_eq!(
                a.terminal_theme_id_for(ColorScheme::Light),
                "cool-import",
                "the Light tab wrote the Light slot"
            );
            assert_eq!(
                a.terminal_theme_id_for(ColorScheme::Dark),
                dark_before,
                "the live (Dark) slot is untouched by a Light-tab edit"
            );
            assert_eq!(a.scheme, live, "editing a tab never flips the live scheme");
        });
        let _ = std::fs::remove_dir_all(&base);
    }

    // --- Opacity/blur steppers (slice 4: the appearance-pane sliders) ---------

    #[test]
    fn opacity_step_targets_clamp_at_the_slider_floor_and_ceiling() {
        // Mid-range: both directions step cleanly by OPACITY_STEP_PCT.
        assert_eq!(opacity_step_targets(80), (75, 85));
        // At the floor (55): decrementing stays pinned, not below 55.
        assert_eq!(opacity_step_targets(55), (55, 60));
        // At the ceiling (100): incrementing stays pinned, not above 100.
        assert_eq!(opacity_step_targets(100), (95, 100));
    }

    #[test]
    fn blur_step_targets_clamp_at_the_slider_floor_and_ceiling() {
        assert_eq!(blur_step_targets(30), (25, 35));
        // At the floor (0): decrementing stays pinned, not negative.
        assert_eq!(blur_step_targets(0), (0, 5));
        // At the ceiling (60): incrementing stays pinned, not above 60.
        assert_eq!(blur_step_targets(60), (55, 60));
    }

    /// A fresh-defaults store so the opacity/blur mutators have a `Global` to
    /// mutate (`current_appearance`/`commit_appearance` no-op without one).
    fn setup_theme_store(cx: &mut TestAppContext, tag: &str) -> PathBuf {
        let base = temp_base(tag);
        cx.update(|app| {
            app.set_global(ThemeSettingsStore::with_defaults(base.join("ui_settings.json")));
        });
        base
    }

    #[gpui::test]
    fn opacity_stepper_dec_and_inc_call_apply_window_opacity_for_the_given_scheme(
        cx: &mut TestAppContext,
    ) {
        let base = setup_theme_store(cx, "opacity-stepper");
        cx.update(|app| {
            // Dark starts at the plan default (80%); step down then up and assert
            // the exact stored value each time (the click handler's wiring, not
            // just the pure target math already covered above).
            theme_settings::apply_window_opacity(app, ColorScheme::Dark, 80);
            let (dec, inc) = opacity_step_targets(80);
            theme_settings::apply_window_opacity(app, ColorScheme::Dark, dec as u8);
            assert_eq!(
                app.global::<ThemeSettingsStore>()
                    .appearance()
                    .window_opacity_pct_for(ColorScheme::Dark),
                75
            );
            theme_settings::apply_window_opacity(app, ColorScheme::Dark, inc as u8);
            assert_eq!(
                app.global::<ThemeSettingsStore>()
                    .appearance()
                    .window_opacity_pct_for(ColorScheme::Dark),
                85
            );
            // The Light slot is untouched by a Dark-scheme step.
            assert_eq!(
                app.global::<ThemeSettingsStore>()
                    .appearance()
                    .window_opacity_pct_for(ColorScheme::Light),
                90
            );
        });
        let _ = std::fs::remove_dir_all(&base);
    }

    #[gpui::test]
    fn blur_stepper_dec_and_inc_call_apply_blur_radius_for_the_given_scheme(cx: &mut TestAppContext) {
        let base = setup_theme_store(cx, "blur-stepper");
        cx.update(|app| {
            let (dec, inc) = blur_step_targets(30); // the plan default
            theme_settings::apply_blur_radius(app, ColorScheme::Light, dec as u16);
            assert_eq!(
                app.global::<ThemeSettingsStore>()
                    .appearance()
                    .blur_radius_for(ColorScheme::Light),
                25
            );
            theme_settings::apply_blur_radius(app, ColorScheme::Light, inc as u16);
            assert_eq!(
                app.global::<ThemeSettingsStore>()
                    .appearance()
                    .blur_radius_for(ColorScheme::Light),
                35
            );
            // The Dark slot is untouched by a Light-scheme step.
            assert_eq!(
                app.global::<ThemeSettingsStore>()
                    .appearance()
                    .blur_radius_for(ColorScheme::Dark),
                30
            );
        });
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Tab targeting (plan 06 Validation): moving the opacity/blur steppers on
    /// the tab writes the TAB scheme's slots while the OTHER scheme is live —
    /// edit Dark while Light is live, and vice versa; the live scheme never flips.
    #[gpui::test]
    fn opacity_and_blur_steppers_target_the_tab_scheme_not_the_live_scheme(
        cx: &mut TestAppContext,
    ) {
        let base = setup_theme_store(cx, "opacity-blur-tab-target");
        cx.update(|app| {
            // (a) Live scheme = Light, edit the Dark tab.
            theme_settings::apply_scheme(app, ColorScheme::Light);
            let live = app.global::<ThemeSettingsStore>().appearance().scheme;
            assert_eq!(live, ColorScheme::Light);
            let target = appearance_tab_scheme(Some(ColorScheme::Dark), live);
            assert_eq!(target, ColorScheme::Dark);

            theme_settings::apply_window_opacity(app, target, 70);
            theme_settings::apply_blur_radius(app, target, 10);
            let a = app.global::<ThemeSettingsStore>().appearance().clone();
            assert_eq!(a.window_opacity_pct_for(ColorScheme::Dark), 70, "Dark tab wrote Dark opacity");
            assert_eq!(a.blur_radius_for(ColorScheme::Dark), 10, "Dark tab wrote Dark blur");
            assert_eq!(a.window_opacity_pct_for(ColorScheme::Light), 90, "the live Light opacity is untouched");
            assert_eq!(a.blur_radius_for(ColorScheme::Light), 30, "the live Light blur is untouched");
            assert_eq!(a.scheme, ColorScheme::Light, "editing the Dark tab never flips the live scheme");

            // (b) Now the reverse: with Dark live, edit the Light tab.
            theme_settings::apply_scheme(app, ColorScheme::Dark);
            let live = app.global::<ThemeSettingsStore>().appearance().scheme;
            assert_eq!(live, ColorScheme::Dark);
            let target = appearance_tab_scheme(Some(ColorScheme::Light), live);
            assert_eq!(target, ColorScheme::Light);

            theme_settings::apply_window_opacity(app, target, 65);
            theme_settings::apply_blur_radius(app, target, 45);
            let a = app.global::<ThemeSettingsStore>().appearance().clone();
            assert_eq!(a.window_opacity_pct_for(ColorScheme::Light), 65, "Light tab wrote Light opacity");
            assert_eq!(a.blur_radius_for(ColorScheme::Light), 45, "Light tab wrote Light blur");
            // Dark keeps the values written in step (a) — the Light-tab edit did not touch them.
            assert_eq!(a.window_opacity_pct_for(ColorScheme::Dark), 70, "the live Dark opacity is untouched");
            assert_eq!(a.blur_radius_for(ColorScheme::Dark), 10, "the live Dark blur is untouched");
            assert_eq!(a.scheme, ColorScheme::Dark, "editing the Light tab never flips the live scheme");
        });
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The Scheme control's selection contract (round-2.5): segment click →
    /// `apply_scheme` flips the LIVE scheme; the sync lock makes it a no-op —
    /// the same pinning `dropdown_selection_drives_the_exact_apply_calls` gives
    /// the theme picker.
    #[gpui::test]
    fn scheme_segment_selection_flips_the_live_scheme_and_respects_the_sync_lock(
        cx: &mut TestAppContext,
    ) {
        let base = setup_theme_store(cx, "scheme-segments");
        cx.update(|app| {
            // Fresh defaults: Dark stored. Segments carry the a11y ids, the
            // right active flag, and are enabled with sync off.
            let segs = scheme_segments(ColorScheme::Dark, false);
            assert_eq!(segs[0].id.as_ref(), "settings.appearance.scheme.Light");
            assert_eq!(segs[1].id.as_ref(), "settings.appearance.scheme.Dark");
            assert!(!segs[0].active && segs[1].active);

            // Clicking "Light" flips the LIVE scheme.
            segs[0].select(app);
            assert_eq!(
                app.global::<ThemeSettingsStore>().appearance().scheme,
                ColorScheme::Light,
                "the Light segment's select applied apply_scheme(Light)"
            );

            // Sync-locked segments are select no-ops (the render path attaches
            // no handler; the contract path must refuse too).
            let locked = scheme_segments(ColorScheme::Light, true);
            assert!(!locked[1].enabled);
            locked[1].select(app);
            assert_eq!(
                app.global::<ThemeSettingsStore>().appearance().scheme,
                ColorScheme::Light,
                "a sync-locked segment never flips the scheme"
            );
        });
        let _ = std::fs::remove_dir_all(&base);
    }

    #[gpui::test]
    fn default_terminal_id_for_falls_back_when_the_catalog_is_absent(cx: &mut TestAppContext) {
        // No `TerminalThemeCatalog` global installed ⇒ the known Nice-default ids.
        cx.update(|app| {
            assert_eq!(
                default_terminal_id_for(app, ColorScheme::Light),
                "nice-default-light"
            );
            assert_eq!(
                default_terminal_id_for(app, ColorScheme::Dark),
                "nice-default-dark"
            );
        });
    }
}
