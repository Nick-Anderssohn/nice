//! The terminal-theme catalog — the id → [`nice_term_view::TerminalTheme`]
//! resolution seam R22 fills.
//!
//! R21 ships a **stub**: only the two Nice built-ins
//! (`nice-default-light` / `nice-default-dark`, ported from
//! `BuiltInTerminalThemes.swift`). The persisted terminal-theme ids default to
//! the Catppuccin ids (`catppuccin-latte` / `catppuccin-mocha`, Swift parity),
//! which are NOT in this stub — so they resolve through the **unknown-id →
//! Nice-default fallback** below until R22 adds the full 12-theme table +
//! Ghostty-imported files. That fill is purely additive: **R22 replaces the
//! built-in table WITHOUT changing [`TerminalThemeCatalog::resolve`] /
//! [`TerminalThemeCatalog::themes`] signatures** (Exported contracts, R21 plan).
//!
//! Boundary note (TRANCHE-2-NOTES §4): the catalog is an **app-crate** concern —
//! it composes catalog metadata (id / display name / scope) over the
//! render-subset [`nice_term_view::TerminalTheme`], which itself carries no
//! catalog fields. Nothing here leaks into `nice-term-*`.

#![allow(dead_code)] // Slice 2/3 (ThemeState) + R22/R23 consume this seam.

use gpui::Global;
use nice_term_view::TerminalTheme;
use nice_theme::ColorScheme;

/// Whether a theme is designed for one scheme or works in either. Ported from
/// `TerminalTheme.Scope` (`TerminalTheme.swift:99-108`). Imported themes default
/// to [`ThemeScope::Either`] (the file format records no authorial intent) — R22.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeScope {
    /// Designed for light mode; appears only in the light-mode picker.
    Light,
    /// Designed for dark mode; appears only in the dark-mode picker.
    Dark,
    /// Works in either scheme; appears in both pickers.
    Either,
}

impl ThemeScope {
    /// Whether a theme with this scope belongs in the picker for `scheme`.
    /// `.either` matches both sides. Verbatim from `TerminalTheme.matches`
    /// (`TerminalTheme.swift:117-124`).
    pub fn matches(self, scheme: ColorScheme) -> bool {
        match (self, scheme) {
            (ThemeScope::Either, _) => true,
            (ThemeScope::Light, ColorScheme::Light) => true,
            (ThemeScope::Dark, ColorScheme::Dark) => true,
            _ => false,
        }
    }
}

/// One picker row: the metadata half of a catalog theme (the render half is the
/// [`TerminalTheme`] the catalog resolves to). R23's per-scheme picker renders a
/// list of these; R22 extends the set with the other 10 built-ins + imports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogEntry {
    /// The stable id persisted in the `appearance` section
    /// (`terminal_theme_light_id` / `terminal_theme_dark_id`).
    pub id: String,
    /// The human-facing name for the picker.
    pub display_name: String,
    /// Which scheme(s) this theme is offered in.
    pub scope: ThemeScope,
}

/// One built-in theme: a [`CatalogEntry`]'s metadata paired with the concrete
/// render theme it resolves to.
#[derive(Clone, Debug)]
struct BuiltIn {
    id: &'static str,
    display_name: &'static str,
    scope: ThemeScope,
    theme: TerminalTheme,
}

/// The process-wide terminal-theme catalog gpui `Global`. R21 carries only the
/// two Nice built-ins; R22 fills it with the full built-in table + imported
/// files under `<support-root>/Nice RS Dev/terminal-themes/` (the
/// `NICE_APPLICATION_SUPPORT_ROOT` + `STORE_FOLDER` convention) — additively,
/// behind the same [`resolve`](Self::resolve) / [`themes`](Self::themes) seam.
pub struct TerminalThemeCatalog {
    builtins: Vec<BuiltIn>,
}

impl Global for TerminalThemeCatalog {}

impl TerminalThemeCatalog {
    /// The R21 stub: the two Nice built-ins. `run_selftest` and `app::run` both
    /// install this (slice 3); R22 swaps the body for the full table.
    pub fn with_builtins() -> Self {
        Self {
            builtins: vec![
                BuiltIn {
                    id: "nice-default-light",
                    display_name: "Nice Default (Light)",
                    scope: ThemeScope::Light,
                    theme: TerminalTheme::nice_default_light(),
                },
                BuiltIn {
                    id: "nice-default-dark",
                    display_name: "Nice Default (Dark)",
                    scope: ThemeScope::Dark,
                    theme: TerminalTheme::nice_default_dark(),
                },
            ],
        }
    }

    /// Resolve a persisted terminal-theme `id` for `scheme` to a concrete
    /// [`TerminalTheme`]. A **known id whose scope matches** `scheme` resolves to
    /// that theme; **any other id** (unknown, not-yet-added, deleted-imported, or
    /// an id whose scope does not match) falls back to the Nice default for
    /// `scheme` (`nice_default_light()` / `nice_default_dark()`).
    ///
    /// This is a **deliberate one-level** fallback (R21 plan / dossier §2): Swift's
    /// `effectiveTerminalTheme` is two-level (`id → default-id → nice_default`,
    /// `Tweaks.swift:668-693`), so a stray id renders the Nice default rather than
    /// the Catppuccin default. It is exactly what makes R21's Catppuccin default
    /// ids render Nice defaults until R22 adds those themes (R22-additive).
    pub fn resolve(&self, id: &str, scheme: ColorScheme) -> TerminalTheme {
        for b in &self.builtins {
            if b.id == id && b.scope.matches(scheme) {
                return b.theme.clone();
            }
        }
        match scheme {
            ColorScheme::Light => TerminalTheme::nice_default_light(),
            ColorScheme::Dark => TerminalTheme::nice_default_dark(),
        }
    }

    /// The ordered picker list for one `scheme`: every built-in whose scope
    /// matches, in declaration order (R23's picker). R21 returns the single
    /// matching Nice built-in per scheme.
    pub fn themes(&self, scheme: ColorScheme) -> Vec<CatalogEntry> {
        self.builtins
            .iter()
            .filter(|b| b.scope.matches(scheme))
            .map(|b| CatalogEntry {
                id: b.id.to_string(),
                display_name: b.display_name.to_string(),
                scope: b.scope,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_matches_mirrors_swift() {
        // TerminalTheme.swift:117-124.
        assert!(ThemeScope::Light.matches(ColorScheme::Light));
        assert!(!ThemeScope::Light.matches(ColorScheme::Dark));
        assert!(ThemeScope::Dark.matches(ColorScheme::Dark));
        assert!(!ThemeScope::Dark.matches(ColorScheme::Light));
        assert!(ThemeScope::Either.matches(ColorScheme::Light));
        assert!(ThemeScope::Either.matches(ColorScheme::Dark));
    }

    #[test]
    fn resolve_known_id_returns_that_theme() {
        let cat = TerminalThemeCatalog::with_builtins();
        assert_eq!(
            cat.resolve("nice-default-light", ColorScheme::Light),
            TerminalTheme::nice_default_light()
        );
        assert_eq!(
            cat.resolve("nice-default-dark", ColorScheme::Dark),
            TerminalTheme::nice_default_dark()
        );
    }

    /// The R22-additive guarantee: the Catppuccin default ids R21 persists are
    /// NOT in the stub catalog, so they resolve to the Nice default for the
    /// scheme (the deliberate one-level fallback) — R22 makes them concrete
    /// without any default-flip.
    #[test]
    fn resolve_unknown_id_falls_back_to_nice_default() {
        let cat = TerminalThemeCatalog::with_builtins();
        // The persisted fresh-install ids are unknown to the stub.
        assert_eq!(
            cat.resolve("catppuccin-latte", ColorScheme::Light),
            TerminalTheme::nice_default_light()
        );
        assert_eq!(
            cat.resolve("catppuccin-mocha", ColorScheme::Dark),
            TerminalTheme::nice_default_dark()
        );
        // A wholly unknown id, and an empty id, also fall back per scheme.
        assert_eq!(
            cat.resolve("does-not-exist", ColorScheme::Dark),
            TerminalTheme::nice_default_dark()
        );
        assert_eq!(
            cat.resolve("", ColorScheme::Light),
            TerminalTheme::nice_default_light()
        );
    }

    /// A known id whose scope does NOT match the scheme falls back (resolve is
    /// scope-aware, per the Exported contract).
    #[test]
    fn resolve_known_id_off_scheme_falls_back() {
        let cat = TerminalThemeCatalog::with_builtins();
        // nice-default-light is scope Light; resolving it for Dark falls back.
        assert_eq!(
            cat.resolve("nice-default-light", ColorScheme::Dark),
            TerminalTheme::nice_default_dark()
        );
        assert_eq!(
            cat.resolve("nice-default-dark", ColorScheme::Light),
            TerminalTheme::nice_default_light()
        );
    }

    #[test]
    fn themes_for_returns_the_matching_nice_entry_per_scheme() {
        let cat = TerminalThemeCatalog::with_builtins();

        let light = cat.themes(ColorScheme::Light);
        assert_eq!(light.len(), 1);
        assert_eq!(light[0].id, "nice-default-light");
        assert_eq!(light[0].display_name, "Nice Default (Light)");
        assert_eq!(light[0].scope, ThemeScope::Light);

        let dark = cat.themes(ColorScheme::Dark);
        assert_eq!(dark.len(), 1);
        assert_eq!(dark[0].id, "nice-default-dark");
        assert_eq!(dark[0].display_name, "Nice Default (Dark)");
        assert_eq!(dark[0].scope, ThemeScope::Dark);
    }
}
